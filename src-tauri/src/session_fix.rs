//! DSH 会话版本兼容修复：扫描 / 修复 / 截断 / 备份回滚。
//!
//! 背景：DSH 用事件溯源存会话（`session.v4.jsonl.zstd`）。高版本向会话写入
//! 低版本未知且未标 `ignorable` 的事件后，低版本加载直接抛
//! `SessionQueryError`（官方 fail-closed 校验，见 `known-event-types.js`）。
//!
//! 文件格式：**多帧 zstd**——首帧=会话日志头，之后每帧=一批事件（追加式）。
//! 修复必须按帧重写：只重压真正被改的帧，未动帧原字节拷贝（校验和保持有效），
//! 整体重压成单帧会破坏引擎依赖的帧布局（断尾恢复按帧粒度工作）。
//!
//! 安全边界（对齐 dsh-session-compat-fix 插件）：
//! - 只对「目标词汇表未知 且 ignorable !== true」的事件做字符串级追加
//!   `,"ignorable":true`（保留原字节序、seq、其他字段）；罕见 ignorable:false
//!   时用 serde 重写该行（键序可能变化，语义不变）
//! - 首行 `type:"session"` 日志头由引擎单独处理，永远豁免
//! - 活跃保护：近期（90 秒）被写入过的会话自动跳过，防并发写坏
//! - 修复前整文件备份 + manifest.json（sha256/来源路径/未知类型）；
//!   回滚按清单精确还原，且回滚前先把当前状态再存一份（回滚可撤销）
//! - 幂等：已标记 true 的事件不再动；无改动时不备份不写盘
//! - 截断回滚（破坏性兜底）：删除第一个未知事件及其后全部内容，仅修复无效时用
//! - 内置 0.2.0/0.2.1 词汇表兜底：本机未探测到任何 DSH 版本时也能修

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// 活跃判定阈值：90 秒内被写入过的会话视为「可能正在使用」
pub const ACTIVE_WINDOW_SECS: u64 = 90;

/// 会话日志文件名：session.v4.jsonl.zstd / 兼容将来的 session.v5…/ 无版本号
fn is_session_log_name(name: &str) -> bool {
    let n = name.to_lowercase();
    n == "session.jsonl.zstd"
        || (n.starts_with("session.v") && n.ends_with(".jsonl.zstd") && n.contains(".jsonl."))
}

// ==================== 多帧 zstd 编解码 ====================

/// `0xFD2FB528` little-endian，Zstandard 帧魔数
const ZSTD_MAGIC: u32 = 0xFD2FB528;

struct FrameScan {
    /// 每个完整帧的 [start, end) 字节区间
    frames: Vec<(usize, usize)>,
    /// 尾部不完整帧的起始偏移（正在写入/上次异常退出时存在）
    torn_start: Option<usize>,
}

/// 逐帧扫描多帧 zstd 会话日志。
///
/// 镜像引擎 `scanZstdFrames` 的魔数/帧头位布局/块遍历/保留位拒绝——
/// 本函数接受的日志就是引擎接受的日志。
fn scan_zstd_frames(buf: &[u8]) -> Result<FrameScan, String> {
    let mut frames = Vec::new();
    let mut off = 0usize;
    let len = buf.len();
    let torn = |frames: Vec<(usize, usize)>, start: usize| {
        Ok(FrameScan {
            frames,
            torn_start: Some(start),
        })
    };
    while off < len {
        let start = off;
        if len - off < 4 {
            return torn(frames, start);
        }
        let magic = u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
        if magic != ZSTD_MAGIC {
            return Err(format!("损坏的 zstd 会话日志：字节 {off} 处帧魔数不符"));
        }
        off += 4;
        if off == len {
            return torn(frames, start);
        }
        let descriptor = buf[off];
        off += 1;
        if descriptor & 0b0001_1000 != 0 {
            return Err(format!(
                "损坏的 zstd 会话日志：字节 {} 处帧头保留位非零",
                off - 1
            ));
        }
        let content_size_flag = descriptor >> 6;
        let single_segment = descriptor & 0b0010_0000 != 0;
        let checksum_flag = descriptor & 0b0000_0100 != 0;
        let dict_flag = descriptor & 0b0000_0011;
        let dict_bytes = if dict_flag == 3 { 4 } else { dict_flag as usize };
        let content_size_bytes = if content_size_flag == 0 {
            if single_segment {
                1
            } else {
                0
            }
        } else {
            1usize << content_size_flag
        };
        let remaining = (if single_segment { 0 } else { 1 }) + dict_bytes + content_size_bytes;
        if len - off < remaining {
            return torn(frames, start);
        }
        off += remaining;
        loop {
            if len - off < 3 {
                return torn(frames, start);
            }
            let bh = u32::from(buf[off]) | u32::from(buf[off + 1]) << 8 | u32::from(buf[off + 2]) << 16;
            off += 3;
            let last = bh & 1 != 0;
            let block_type = (bh >> 1) & 3;
            let block_size = (bh >> 3) as usize;
            if block_type == 3 {
                return Err(format!(
                    "损坏的 zstd 会话日志：字节 {} 处块类型保留值",
                    off - 3
                ));
            }
            let payload = if block_type == 1 { 1 } else { block_size };
            if len - off < payload {
                return torn(frames, start);
            }
            off += payload;
            if last {
                break;
            }
        }
        if checksum_flag {
            if len - off < 4 {
                return torn(frames, start);
            }
            off += 4;
        }
        frames.push((start, off));
    }
    Ok(FrameScan {
        frames,
        torn_start: None,
    })
}

/// 解压单个完整帧（zstd 校验和由解码器执行）
fn decompress_frame(frame: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    zstd::stream::read::Decoder::new(Cursor::new(frame))
        .and_then(|mut d| d.read_to_end(&mut out))
        .map_err(|e| format!("帧解压失败: {e}"))?;
    Ok(out)
}

/// 以引擎同参数（带校验和）压缩一帧
fn compress_frame(plain: &[u8]) -> Result<Vec<u8>, String> {
    let mut enc = zstd::stream::Encoder::new(Vec::new(), 3)
        .map_err(|e| format!("帧压缩失败: {e}"))?;
    enc.include_checksum(true)
        .map_err(|e| format!("帧压缩失败: {e}"))?;
    enc.write_all(plain).map_err(|e| format!("帧压缩失败: {e}"))?;
    enc.finish().map_err(|e| format!("帧压缩失败: {e}"))
}

struct DecodedLog {
    frames: Vec<(usize, usize)>,
    torn_start: Option<usize>,
    /// 每帧的行（保持原行文本，空行剔除）
    frame_lines: Vec<Vec<String>>,
    /// 每帧原文是否以 \n 结尾（重建时保持）
    frame_trailing_nl: Vec<bool>,
}

/// 逐帧解码：完整帧全部解出；尾部不完整帧忽略（torn_start 记录断点）
fn decode_session_log(buf: &[u8]) -> Result<DecodedLog, String> {
    let scan = scan_zstd_frames(buf)?;
    let mut frame_lines = Vec::with_capacity(scan.frames.len());
    let mut frame_trailing_nl = Vec::with_capacity(scan.frames.len());
    for (s, e) in &scan.frames {
        let text = String::from_utf8_lossy(&decompress_frame(&buf[*s..*e])?).to_string();
        frame_trailing_nl.push(text.ends_with('\n'));
        frame_lines.push(
            text.split('\n')
                .filter(|l| !l.trim().is_empty())
                .map(String::from)
                .collect(),
        );
    }
    Ok(DecodedLog {
        frames: scan.frames,
        torn_start: scan.torn_start,
        frame_lines,
        frame_trailing_nl,
    })
}

/// 重建一帧的文本（行序/换行风格与原文一致）
fn rebuild_frame_text(lines: &[String], trailing_nl: bool) -> String {
    let mut t = lines.join("\n");
    if trailing_nl && !lines.is_empty() {
        t.push('\n');
    }
    t
}

fn line_type(line: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(line.trim())
        .ok()?
        .get("type")?
        .as_str()
        .map(String::from)
}

fn line_ignorable(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line.trim())
        .ok()
        .and_then(|v| v.get("ignorable").and_then(|x| x.as_bool()))
        == Some(true)
}

/// user/message 事件的展示文本（data.message.content / data.content；字符串或 text 部件数组）
fn message_text(v: &serde_json::Value) -> String {
    let d = v.get("data");
    let content = d
        .and_then(|x| x.get("message"))
        .and_then(|m| m.get("content"))
        .or_else(|| d.and_then(|x| x.get("content")))
        .or_else(|| v.get("message").and_then(|m| m.get("content")));
    let Some(c) = content else {
        return String::new();
    };
    if let Some(s) = c.as_str() {
        return s.trim().to_string();
    }
    if let Some(arr) = c.as_array() {
        let parts: Vec<&str> = arr
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(|x| x.as_str()) == Some("text") {
                    p.get("text").and_then(|x| x.as_str())
                } else {
                    None
                }
            })
            .collect();
        return parts.join("\n").trim().to_string();
    }
    String::new()
}

/// 引擎准入复查：未知且未标 ignorable 的事件数（首行日志头豁免）
fn count_blocking(log: &DecodedLog, known: &HashSet<String>) -> u64 {
    let mut n = 0u64;
    for (fi, fls) in log.frame_lines.iter().enumerate() {
        for (li, l) in fls.iter().enumerate() {
            let Some(t) = line_type(l) else { continue };
            if fi == 0 && li == 0 && t == "session" {
                continue;
            }
            if known.contains(&t) || line_ignorable(l) {
                continue;
            }
            n += 1;
        }
    }
    n
}

// ==================== 校验和 / 原子写 ====================

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// 临时文件 + 原子重命名，中断不留下半个文件
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("dshpm-tmp");
    fs::write(&tmp, bytes).map_err(|e| format!("写入临时文件失败: {e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("替换会话文件失败: {e}"))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ==================== 词汇表（known-event-types.js + 内置兜底） ====================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VocabInfo {
    /// dsh-session 包版本（package.json）
    pub version: String,
    /// 事件类型数
    pub count: usize,
    /// 来源（文件路径或「内置清单」）
    pub source: String,
    /// 内置清单（非本机探测）
    pub builtin: bool,
}

/// 一个版本词汇表的完整记录（类型列表随版本走，内置清单无源文件可解析）
#[derive(Debug, Clone)]
pub struct VocabEntry {
    pub info: VocabInfo,
    pub types: Vec<String>,
}

/// 内置 0.2.0 词汇表（59 种，逐字摘自 dsh-session-compat-fix/lib/catalog.js）
pub const BUILTIN_020: &[&str] = &[
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "assistant/attempt",
    "assistant/message",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "deliverables/presented",
    "developer/message",
    "feedback/message-delete",
    "feedback/message-put",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "image/offload",
    "llm/retry",
    "llm/retry-started",
    "model/selection",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session-log-deepseek/delivery-accepted",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/catalog",
    "subagent/descriptor",
    "subagent/model-selection-policy",
    "system/message",
    "team/member",
    "team/message/delivered",
    "team/message/queued",
    "team/task",
    "todo/write",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    "tool/call",
    "tool/ptc-dispatch",
    "tool/ptc-dispatch-start",
    "tool/result",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
    "workspace/changes",
];

/// 0.2.1 相对 0.2.0 新增（合计 62 种）
pub const BUILTIN_021_EXTRA: &[&str] = &["job/registered", "job/settled", "working-directory/change"];

/// 内置词汇表兜底：本机一个 DSH 版本都没探测到时也能修
pub fn builtin_vocabs() -> Vec<VocabEntry> {
    let v020: Vec<String> = BUILTIN_020.iter().map(|s| s.to_string()).collect();
    let mut v021 = v020.clone();
    v021.extend(BUILTIN_021_EXTRA.iter().map(|s| s.to_string()));
    let mk = |ver: &str, types: Vec<String>| VocabEntry {
        info: VocabInfo {
            version: ver.to_string(),
            count: types.len(),
            source: "内置清单".to_string(),
            builtin: true,
        },
        types,
    };
    vec![mk("0.2.0", v020), mk("0.2.1", v021)]
}

/// 有界探测词汇表文件：不递归走树（~/.dsh 下 node_modules 巨树会走几十秒），
/// 而是按确定性相对路径直接探测：根自身 / 一级子目录 / 二级子目录 拼包路径。
fn find_vocab_files(root: &Path, out: &mut Vec<PathBuf>) {
    const REL: &str = "node_modules/@deepseek-ai/dsh-session/lib/types/known-event-types.js";
    let probe = |dir: &Path, out: &mut Vec<PathBuf>| {
        let p = dir.join(REL);
        if p.is_file() && p.to_string_lossy().replace('\\', "/").contains("node_modules/@deepseek-ai/dsh-session/") {
            out.push(p);
        }
    };
    probe(root, out);
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        probe(&p, out); // root/<x>/node_modules/...
        if let Ok(sub) = fs::read_dir(&p) {
            for e2 in sub.flatten() {
                if e2.path().is_dir() {
                    probe(&e2.path(), out); // root/<x>/<y>/node_modules/...
                }
            }
        }
    }
}

/// 从 known-event-types.js 提取 `KNOWN_SESSION_EVENT_TYPES = new Set([ 'a/b', … ])` 内的单引号串
pub fn parse_known_types(js_path: &Path) -> Result<Vec<String>, String> {
    let text = fs::read_to_string(js_path).map_err(|e| format!("读取词汇表失败: {e}"))?;
    let start = text
        .find("KNOWN_SESSION_EVENT_TYPES")
        .ok_or_else(|| "词汇表文件缺少 KNOWN_SESSION_EVENT_TYPES".to_string())?;
    let set_open = text[start..]
        .find('[')
        .map(|i| start + i)
        .ok_or_else(|| "词汇表文件缺少 Set([".to_string())?;
    let set_close = text[set_open..]
        .find("])")
        .map(|i| set_open + i)
        .ok_or_else(|| "词汇表 Set 未闭合".to_string())?;
    let block = &text[set_open..set_close];
    let chars: Vec<char> = block.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\'' {
            let mut s = String::new();
            i += 1;
            while i < chars.len() {
                let c = chars[i];
                i += 1;
                if c == '\'' {
                    break;
                }
                if c == '\\' && i < chars.len() {
                    s.push(chars[i]);
                    i += 1;
                    continue;
                }
                s.push(c);
            }
            if !s.trim().is_empty() {
                out.push(s);
            }
        } else {
            i += 1;
        }
    }
    if out.is_empty() {
        return Err("词汇表 Set 为空或解析失败".to_string());
    }
    Ok(out)
}

/// 词汇表来源包版本：同包的 package.json（…/node_modules/@deepseek-ai/dsh-session/package.json）
fn vocab_package_version(js_path: &Path) -> String {
    let pkg = js_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|p| p.join("package.json"));
    if let Some(pkg) = pkg {
        if let Ok(text) = fs::read_to_string(&pkg) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
                    return ver.to_string();
                }
            }
        }
    }
    // 兜底：父目录名（…/node_modules/@deepseek-ai/dsh-session）
    js_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 合并词汇表：内置 0.2.0/0.2.1 兜底 + 本机探测（同版本内置优先，不被探测结果覆盖）
pub fn merged_vocabs(roots: &[PathBuf]) -> Vec<VocabEntry> {
    let mut out = builtin_vocabs();
    let mut files = Vec::new();
    for r in roots {
        find_vocab_files(r, &mut files);
    }
    for f in files {
        let Ok(types) = parse_known_types(&f) else {
            continue;
        };
        let ver = vocab_package_version(&f);
        if ver.is_empty() || out.iter().any(|e| e.info.version == ver) {
            continue; // 内置优先
        }
        out.push(VocabEntry {
            info: VocabInfo {
                version: ver,
                count: types.len(),
                source: f.to_string_lossy().to_string(),
                builtin: false,
            },
            types,
        });
    }
    out.sort_by_key(|e| version_key(&e.info.version));
    out
}

/// 版本排序键：核心三段数字 + 预发布段（release 用 `~` 使其排在任何预发布之后）
fn version_key(v: &str) -> (u64, u64, u64, String) {
    let (core, pre) = match v.trim().split_once('-') {
        Some((a, b)) => (a, b.to_string()),
        None => (v.trim(), "~".to_string()),
    };
    let mut nums = [0u64; 3];
    for (i, seg) in core.split('.').enumerate().take(3) {
        nums[i] = seg.parse().unwrap_or(0);
    }
    (nums[0], nums[1], nums[2], pre)
}

/// 选词汇表：精确 → 最近的更低版本（宁多标不漏标）→ 全无更低时用最旧的
pub fn pick_vocab_entry<'a>(entries: &'a [VocabEntry], target: &str) -> Option<&'a VocabEntry> {
    if entries.is_empty() {
        return None;
    }
    if let Some(exact) = entries.iter().find(|e| e.info.version == target) {
        return Some(exact);
    }
    let tk = version_key(target);
    let mut lower: Vec<&VocabEntry> = entries
        .iter()
        .filter(|e| version_key(&e.info.version) < tk)
        .collect();
    lower.sort_by_key(|e| version_key(&e.info.version));
    if let Some(best) = lower.last() {
        return Some(best);
    }
    entries.iter().min_by_key(|e| version_key(&e.info.version))
}

// ==================== 扫描 ====================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnknownEvent {
    pub kind: String,
    pub count: u64,
    /// 受影响 seq（最多记 64 个，详情面板展示用）
    pub seqs: Vec<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionScanItem {
    /// 会话文件绝对路径
    pub path: String,
    /// 会话 UUID（目录名 session-xxx）
    pub session_id: String,
    /// 工作区 slug（sessions 下的一级目录名）
    pub workspace: String,
    /// 会话标题（session/title 事件；兜底首条 user/message 文本，截 80 字符）
    pub title: String,
    /// 事件最大 seq（无 seq 为 -1）
    pub max_seq: i64,
    /// 日志头格式版本（session.v4 → 4；缺失为 null）
    pub format_version: Option<i64>,
    pub lines: u64,
    pub size: u64,
    pub modified: String,
    /// 帧解码失败（损坏；刻意不修）
    pub readable: bool,
    /// 尾部不完整帧（正在写入或上次异常退出；完整帧仍可读可修，修复时断尾丢弃）
    pub torn_tail: bool,
    /// 90 秒内被写入过（可能正在使用；修复/截断会跳过）
    pub active: bool,
    /// 存在「未知且未标 ignorable」事件
    pub faulty: bool,
    pub unknown: Vec<UnknownEvent>,
}

/// 递归收集会话文件（跳过备份目录，防御性：备份正常应放 sessions 兄弟目录）
fn collect_session_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if e.file_name().to_string_lossy() == "dshpm-session-backups" {
                continue;
            }
            collect_session_files(&p, out);
        } else if is_session_log_name(&e.file_name().to_string_lossy()) {
            out.push(p);
        }
    }
}

/// 扫描会话目录（帧级解码）：列出故障会话与未知事件类型；
/// 正在写入的会话能读出完整帧并标 torn_tail；整帧解不开才判不可读（原样保留不修）
pub fn scan_sessions_dir(
    sessions_dir: &Path,
    known: &HashSet<String>,
) -> Result<Vec<SessionScanItem>, String> {
    if !sessions_dir.is_dir() {
        return Err(format!("会话目录不存在: {}", sessions_dir.display()));
    }
    let mut files = Vec::new();
    collect_session_files(sessions_dir, &mut files);
    let now = SystemTime::now();
    let mut items = Vec::new();
    for f in files {
        let meta = fs::metadata(&f).ok();
        let active = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .map(|t| now.duration_since(t).unwrap_or(Duration::ZERO) < Duration::from_secs(ACTIVE_WINDOW_SECS))
            .unwrap_or(false);
        let mut unknown_counts: BTreeMap<String, (u64, Vec<i64>)> = BTreeMap::new();
        let (mut lines, mut readable, mut torn_tail) = (0u64, true, false);
        let (mut title_event, mut title_msg) = (None, None);
        let (mut max_seq, mut format_version) = (-1i64, None);
        match fs::read(&f)
            .map_err(|e| format!("读取失败: {e}"))
            .and_then(|b| decode_session_log(&b))
        {
            Ok(log) => {
                if log.frames.is_empty() {
                    readable = false; // 一个完整帧都没有
                }
                torn_tail = log.torn_start.is_some();
                for (fi, fls) in log.frame_lines.iter().enumerate() {
                    for (li, l) in fls.iter().enumerate() {
                        lines += 1;
                        let parsed = serde_json::from_str::<serde_json::Value>(l.trim()).ok();
                        if let Some(s) = parsed
                            .as_ref()
                            .and_then(|v| v.get("seq"))
                            .and_then(|x| x.as_i64())
                        {
                            if s > max_seq {
                                max_seq = s;
                            }
                        }
                        let Some(t) = parsed
                            .as_ref()
                            .and_then(|v| v.get("type"))
                            .and_then(|x| x.as_str())
                            .map(String::from)
                        else {
                            continue;
                        };
                        // 日志头（首行 type=session）由引擎加载器单独处理，不参与事件白名单
                        if fi == 0 && li == 0 && t == "session" {
                            format_version = parsed
                                .as_ref()
                                .and_then(|v| v.get("version"))
                                .and_then(|x| x.as_i64());
                            continue;
                        }
                        // 展示标题：session/title 事件优先（与插件 readTitle 一致，按类型而非顺序），
                        // 兜底首条 user/message 文本
                        if title_event.is_none() && t == "session/title" {
                            if let Some(tt) = parsed
                                .as_ref()
                                .and_then(|v| v.get("data"))
                                .and_then(|d| d.get("title"))
                                .and_then(|x| x.as_str())
                            {
                                let s = tt.trim();
                                if !s.is_empty() {
                                    title_event = Some(s.chars().take(80).collect::<String>());
                                }
                            }
                        } else if title_msg.is_none() && t == "user/message" {
                            let text = message_text(parsed.as_ref().unwrap());
                            if !text.is_empty() {
                                title_msg = Some(text.chars().take(80).collect::<String>());
                            }
                        }
                        if known.contains(&t) || line_ignorable(l) {
                            continue;
                        }
                        let seq = parsed
                            .as_ref()
                            .and_then(|v| v.get("seq"))
                            .and_then(|x| x.as_i64())
                            .unwrap_or(-1);
                        let e = unknown_counts.entry(t.clone()).or_insert((0, Vec::new()));
                        e.0 += 1;
                        if e.1.len() < 64 {
                            e.1.push(seq);
                        }
                    }
                }
            }
            _ => readable = false,
        }
        let unknown: Vec<UnknownEvent> = unknown_counts
            .into_iter()
            .map(|(kind, (count, seqs))| UnknownEvent { kind, count, seqs })
            .collect();
        let modified = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .map(|t| t.datetime_hms())
            .unwrap_or_default();
        let title = title_event.or(title_msg).unwrap_or_default();
        items.push(SessionScanItem {
            path: f.to_string_lossy().to_string(),
            session_id: f
                .parent()
                .and_then(|p| p.file_name())
                .map(|x| x.to_string_lossy().to_string())
                .unwrap_or_default(),
            workspace: f
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .map(|x| x.to_string_lossy().to_string())
                .unwrap_or_default(),
            title,
            max_seq,
            format_version,
            lines,
            size: meta.map(|m| m.len()).unwrap_or(0),
            modified,
            readable,
            torn_tail,
            active,
            faulty: readable && !unknown.is_empty(),
            unknown,
        });
    }
    items.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.path.cmp(&b.path)));
    Ok(items)
}

// 时间格式化（不引 chrono）：SystemTime → "YYYY-MM-DD HH:MM"
trait TimeHms {
    fn datetime_hms(&self) -> String;
}
impl TimeHms for std::time::SystemTime {
    fn datetime_hms(&self) -> String {
        // 粗略换算（本地时区由前端展示无强需求，这里用 UTC 比较序足够；展示用文件管理器时间也有）
        let secs = self
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // 用简化的天数→日期换算（civil_from_days 算法）
        let days = (secs / 86400) as i64;
        let (y, m, d) = civil_from_days(days);
        let rem = secs % 86400;
        format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, (rem % 3600) / 60)
    }
}
// Howard Hinnant 的 days_from_civil 逆运算
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ==================== 备份清单 ====================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFileEntry {
    /// 被备份的会话文件绝对路径
    pub source_path: String,
    /// 备份集内的文件名（<会话目录名>-<原文件名>，session UUID 全局唯一不撞名）
    pub backup_name: String,
    pub sha256: String,
    pub bytes: u64,
    /// 本次修复/截断涉及的事件类型
    pub unknown_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupManifest {
    pub schema_version: u32,
    /// 毫秒时间戳
    pub created_at: u64,
    /// pre-repair | pre-truncate | pre-restore
    pub trigger: String,
    pub target_version: String,
    pub files: Vec<BackupFileEntry>,
}

fn write_manifest(dir: &Path, manifest: &BackupManifest) -> Result<(), String> {
    let text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    fs::write(dir.join("manifest.json"), format!("{text}\n"))
        .map_err(|e| format!("写备份清单失败: {e}"))
}

fn read_manifest(set_dir: &Path) -> Option<BackupManifest> {
    let text = fs::read_to_string(set_dir.join("manifest.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// 备份集目录名：毫秒时间戳（防同一毫秒内连续操作撞集合）
fn set_dir_name() -> String {
    format!("{}", now_millis())
}

/// 备份集内文件名：<会话目录名>-<原文件名>
fn backup_name_for(path: &Path) -> String {
    let fname = path
        .file_name()
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or_else(|| "session.log".to_string());
    match path.parent().and_then(|p| p.file_name()) {
        Some(d) => format!("{}-{fname}", d.to_string_lossy()),
        None => fname,
    }
}

// ==================== 修复 ====================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixOutcome {
    pub path: String,
    /// fixed（已补标记）| unchanged（无故障，幂等）| skipped（活跃保护）| failed
    pub status: String,
    /// 实际补标记的事件行数（status=fixed 时 >0）
    pub fixed_lines: u64,
    /// skipped/failed 原因
    pub reason: String,
    /// 修复后重读文件按引擎准入规则复查通过
    pub verified: bool,
}

/// 单行修复决策：返回 None 表示不动
fn fix_line(line: &str, known: &HashSet<String>) -> Option<String> {
    let trimmed = line.trim();
    let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return None;
    };
    let obj = v.as_object()?;
    let t = obj.get("type").and_then(|x| x.as_str())?;
    if known.contains(t) {
        return None;
    }
    if obj.get("ignorable").and_then(|x| x.as_bool()) == Some(true) {
        return None; // 幂等：已标记
    }
    if obj.contains_key("ignorable") {
        // 罕见：存在但非 true → serde 重写该行（值不丢；键序可能变化，语义不变）
        let mut obj = obj.clone();
        obj.insert("ignorable".into(), serde_json::Value::Bool(true));
        return Some(serde_json::to_string(&obj).ok()?);
    }
    // 常规：字符串级在末尾 `}` 前插入，原字节序/seq 全保留
    let last = trimmed.rfind('}')?;
    // 有 type 字段 → 非空对象 → 逗号插入合法（{} 不可能带 type）
    Some(format!("{},\"ignorable\":true}}", &trimmed[..last]))
}

/// 逐帧逐行做修复决策：返回（新帧行，改动行数，被改事件类型）
fn process_lines(
    log: &DecodedLog,
    known: &HashSet<String>,
) -> (Vec<Vec<String>>, u64, Vec<String>) {
    let mut out: Vec<Vec<String>> = Vec::with_capacity(log.frame_lines.len());
    let mut changed = 0u64;
    let mut unknown_types: Vec<String> = Vec::new();
    for (fi, fls) in log.frame_lines.iter().enumerate() {
        let mut new_lines = Vec::with_capacity(fls.len());
        for (li, l) in fls.iter().enumerate() {
            // 首行 session 日志头不经白名单，原样保留
            if fi == 0 && li == 0 && line_type(l).as_deref() == Some("session") {
                new_lines.push(l.clone());
                continue;
            }
            match fix_line(l, known) {
                Some(new) => {
                    changed += 1;
                    if let Some(t) = line_type(l) {
                        if !unknown_types.contains(&t) {
                            unknown_types.push(t);
                        }
                    }
                    new_lines.push(new);
                }
                None => new_lines.push(l.clone()),
            }
        }
        out.push(new_lines);
    }
    (out, changed, unknown_types)
}

/// 备份原文件字节到备份集（只在真正改动前调用）
fn backup_bytes(
    backup_dir: &Path,
    path: &Path,
    bytes: &[u8],
    unknown_types: Vec<String>,
) -> Result<BackupFileEntry, String> {
    fs::create_dir_all(backup_dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let backup_name = backup_name_for(path);
    fs::write(backup_dir.join(&backup_name), bytes).map_err(|e| format!("备份失败: {e}"))?;
    Ok(BackupFileEntry {
        source_path: path.to_string_lossy().to_string(),
        backup_name,
        sha256: sha256_hex(bytes),
        bytes: bytes.len() as u64,
        unknown_types,
    })
}

/// 修复单个文件（帧级）：活跃保护 → 帧解码 → 逐行决策 → 只重压被改帧 →
/// 备份 → 原子替换 → 重读复查。任何失败都折算进 outcome（不中断批量）。
fn fix_one(
    path: &Path,
    backup_dir: &Path,
    known: &HashSet<String>,
    active_window: Duration,
) -> (FixOutcome, Option<BackupFileEntry>) {
    let pstr = path.to_string_lossy().to_string();
    match fix_one_inner(path, backup_dir, known, active_window) {
        Ok(r) => r,
        Err(e) => (
            FixOutcome {
                path: pstr,
                status: "failed".to_string(),
                fixed_lines: 0,
                reason: e,
                verified: false,
            },
            None,
        ),
    }
}

fn fix_one_inner(
    path: &Path,
    backup_dir: &Path,
    known: &HashSet<String>,
    active_window: Duration,
) -> Result<(FixOutcome, Option<BackupFileEntry>), String> {
    let pstr = path.to_string_lossy().to_string();
    let outcome = |status: &str, fixed: u64, reason: &str, verified: bool| FixOutcome {
        path: pstr.clone(),
        status: status.to_string(),
        fixed_lines: fixed,
        reason: reason.to_string(),
        verified,
    };
    // 活跃保护：近期被写过的会话可能正被引擎使用，跳过防并发写坏
    if let Some(m) = fs::metadata(path).ok().and_then(|m| m.modified().ok()) {
        let recent = SystemTime::now().duration_since(m).unwrap_or(Duration::ZERO);
        if recent < active_window {
            return Ok((
                outcome("skipped", 0, "会话近期仍在写入，请先关闭该会话再修复", false),
                None,
            ));
        }
    }
    let buf = fs::read(path).map_err(|e| format!("读取失败: {e}"))?;
    let log = decode_session_log(&buf).map_err(|e| format!("解码失败: {e}"))?;
    let (new_lines, changed, unknown_types) = process_lines(&log, known);
    if changed == 0 {
        return Ok((outcome("unchanged", 0, "", true), None));
    }
    let entry = backup_bytes(backup_dir, path, &buf, unknown_types)?;
    // 重建：被改帧重压，未动帧原字节拷贝（校验和保持有效）；断尾丢弃
    let rebuild = (|| -> Result<Vec<u8>, String> {
        let mut rebuilt = Vec::with_capacity(buf.len());
        for (fi, (s, e)) in log.frames.iter().enumerate() {
            if new_lines[fi] == log.frame_lines[fi] {
                rebuilt.extend_from_slice(&buf[*s..*e]);
            } else {
                let text = rebuild_frame_text(&new_lines[fi], log.frame_trailing_nl[fi]);
                rebuilt.extend_from_slice(&compress_frame(text.as_bytes())?);
            }
        }
        Ok(rebuilt)
    })();
    match rebuild.and_then(|r| write_atomic(path, &r)) {
        Ok(()) => {
            // 复查：重读按引擎准入规则验证（描述的必须是实际落盘的内容）
            let verified = fs::read(path)
                .map_err(|e| format!("复查读取失败: {e}"))
                .and_then(|b| decode_session_log(&b))
                .map(|log2| count_blocking(&log2, known) == 0)
                .unwrap_or(false);
            Ok((outcome("fixed", changed, "", verified), Some(entry)))
        }
        Err(e) => Ok((outcome("failed", 0, &e, false), Some(entry))),
    }
}

/// 备份根目录：sessions 的兄弟目录（不进 sessions 树，防引擎扫到）
pub fn backups_root_for(sessions_dir: &Path) -> PathBuf {
    sessions_dir
        .parent()
        .unwrap_or(sessions_dir)
        .join("dshpm-session-backups")
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixResult {
    pub backup_dir: String,
    pub outcomes: Vec<FixOutcome>,
}

/// 批量修复（支持单个）：返回备份集目录与逐文件结果
pub fn fix_sessions(
    paths: &[PathBuf],
    backups_root: &Path,
    known: &HashSet<String>,
    target_version: &str,
    active_window: Duration,
) -> Result<FixResult, String> {
    if paths.is_empty() {
        return Err("未选择要修复的会话".to_string());
    }
    let backup_dir = backups_root.join(set_dir_name());
    let mut outcomes = Vec::new();
    let mut entries = Vec::new();
    for p in paths {
        let (o, e) = fix_one(p, &backup_dir, known, active_window);
        outcomes.push(o);
        if let Some(e) = e {
            entries.push(e);
        }
    }
    if !entries.is_empty() {
        write_manifest(
            &backup_dir,
            &BackupManifest {
                schema_version: 1,
                created_at: now_millis(),
                trigger: "pre-repair".to_string(),
                target_version: target_version.to_string(),
                files: entries,
            },
        )?;
    }
    Ok(FixResult {
        backup_dir: backup_dir.to_string_lossy().to_string(),
        outcomes,
    })
}

// ==================== 回滚 ====================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackFailure {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackResult {
    pub restored: u32,
    /// 回滚前对当前状态做的快照集合（回滚本身可撤销）；空 = 未做快照（legacy）
    pub pre_restore_dir: String,
    pub failed: Vec<RollbackFailure>,
}

/// 回滚最近一次备份集：按 manifest 精确还原 + sha256 校验；
/// 回滚前先把当前状态存一份 pre-restore 快照（回滚本身可撤销）。
/// 全无清单的旧格式集合（按文件名平铺）走 legacy 名字匹配。
pub fn rollback_last(sessions_dir: &Path) -> Result<RollbackResult, String> {
    let backups_root = backups_root_for(sessions_dir);
    let mut sets: Vec<PathBuf> = fs::read_dir(&backups_root)
        .map_err(|e| format!("无备份可回滚（读取 {backups_root:?} 失败: {e}）"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    sets.sort();
    // 最新的带有效清单的集合
    for set in sets.iter().rev() {
        if let Some(m) = read_manifest(set) {
            if !m.files.is_empty() {
                return rollback_with_manifest(&backups_root, set, &m);
            }
        }
    }
    rollback_legacy(sessions_dir, &sets)
}

fn rollback_with_manifest(
    backups_root: &Path,
    set_dir: &Path,
    manifest: &BackupManifest,
) -> Result<RollbackResult, String> {
    // 1) 回滚前先把当前状态存一份（回滚本身可撤销）
    let pre_dir = backups_root.join(set_dir_name());
    let mut pre_entries = Vec::new();
    for f in &manifest.files {
        let src = PathBuf::from(&f.source_path);
        if let Ok(cur) = fs::read(&src) {
            let _ = fs::create_dir_all(&pre_dir);
            if fs::write(pre_dir.join(&f.backup_name), &cur).is_ok() {
                pre_entries.push(BackupFileEntry {
                    source_path: f.source_path.clone(),
                    backup_name: f.backup_name.clone(),
                    sha256: sha256_hex(&cur),
                    bytes: cur.len() as u64,
                    unknown_types: vec![],
                });
            }
        }
    }
    let pre_restore_dir = if pre_entries.is_empty() {
        String::new()
    } else {
        write_manifest(
            &pre_dir,
            &BackupManifest {
                schema_version: 1,
                created_at: now_millis(),
                trigger: "pre-restore".to_string(),
                target_version: manifest.target_version.clone(),
                files: pre_entries,
            },
        )?;
        pre_dir.to_string_lossy().to_string()
    };
    // 2) sha256 校验后按清单精确还原
    let mut restored = 0u32;
    let mut failed = Vec::new();
    for f in &manifest.files {
        let bytes = match fs::read(set_dir.join(&f.backup_name)) {
            Ok(b) => b,
            Err(e) => {
                failed.push(RollbackFailure {
                    path: f.source_path.clone(),
                    reason: format!("备份文件缺失: {e}"),
                });
                continue;
            }
        };
        if sha256_hex(&bytes) != f.sha256 {
            failed.push(RollbackFailure {
                path: f.source_path.clone(),
                reason: "备份 sha256 校验不符（备份可能损坏），已跳过还原".to_string(),
            });
            continue;
        }
        match write_atomic(Path::new(&f.source_path), &bytes) {
            Ok(()) => restored += 1,
            Err(e) => failed.push(RollbackFailure {
                path: f.source_path.clone(),
                reason: e,
            }),
        }
    }
    Ok(RollbackResult {
        restored,
        pre_restore_dir,
        failed,
    })
}

/// 旧格式集合（无清单，备份按会话文件原名平铺）：名字匹配无条件还原
fn rollback_legacy(sessions_dir: &Path, sets: &[PathBuf]) -> Result<RollbackResult, String> {
    let latest = sets.last().ok_or_else(|| "无备份可回滚".to_string())?;
    let mut targets: Vec<PathBuf> = Vec::new();
    collect_session_files(sessions_dir, &mut targets);
    let mut restored = 0u32;
    for e in fs::read_dir(latest)
        .map_err(|e| format!("读备份集失败: {e}"))?
        .flatten()
    {
        let fname = e.file_name().to_string_lossy().to_string();
        if fname == "manifest.json" {
            continue;
        }
        let src = e.path();
        if let Some(dst) = targets.iter().find(|p| {
            p.file_name().map(|x| x.to_string_lossy().to_string()).as_deref() == Some(fname.as_str())
        }) {
            fs::copy(&src, dst).map_err(|e| format!("回滚 {fname} 失败: {e}"))?;
            restored += 1;
        }
    }
    Ok(RollbackResult {
        restored,
        pre_restore_dir: String::new(),
        failed: vec![],
    })
}

// ==================== 单会话备份列表 / 按备份回滚 ====================

/// 毫秒时间戳 → "YYYY-MM-DD HH:MM:SS"（UTC，展示用）
fn format_millis(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86400) as i64;
    let (y, m, d) = civil_from_days(days);
    let rem = secs % 86400;
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// 单个会话的一条备份（跨集合收集）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBackupEntry {
    /// 备份集目录名（毫秒时间戳，restore 定位用）
    pub set_dir: String,
    /// pre-repair | pre-truncate | pre-restore
    pub trigger: String,
    pub created_at: String,
    pub target_version: String,
    pub backup_name: String,
    pub bytes: u64,
}

/// 列出某会话文件的全部备份（跨所有集合，按时间倒序 = 最新在前）
pub fn list_session_backups(
    sessions_dir: &Path,
    source_path: &str,
) -> Result<Vec<SessionBackupEntry>, String> {
    let root = backups_root_for(sessions_dir);
    let Ok(rd) = fs::read_dir(&root) else {
        return Ok(vec![]); // 还没有任何备份
    };
    let mut sets: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    sets.sort();
    let mut out = Vec::new();
    for set in sets {
        let Some(m) = read_manifest(&set) else {
            continue; // 旧格式集合（无清单）不支持按会话定位，跳过
        };
        let set_id = set
            .file_name()
            .map(|x| x.to_string_lossy().to_string())
            .unwrap_or_default();
        for f in &m.files {
            if f.source_path == source_path {
                out.push(SessionBackupEntry {
                    set_dir: set_id.clone(),
                    trigger: m.trigger.clone(),
                    created_at: format_millis(m.created_at),
                    target_version: m.target_version.clone(),
                    backup_name: f.backup_name.clone(),
                    bytes: f.bytes,
                });
            }
        }
    }
    out.reverse();
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOneResult {
    /// 回滚前当前状态的快照集合（可撤销回滚）；空 = 源文件已不存在，未快照
    pub pre_restore_dir: String,
}

/// 回滚单个会话文件到指定备份：清单条目定位 + sha256 校验 + 先快照当前状态。
pub fn restore_session_backup(
    sessions_dir: &Path,
    set_dir_id: &str,
    backup_name: &str,
    source_path: &str,
) -> Result<RestoreOneResult, String> {
    // 防路径穿越：集合目录名必须纯数字；备份文件名不允许分隔符/..
    if set_dir_id.is_empty() || !set_dir_id.chars().all(|c| c.is_ascii_digit()) {
        return Err("非法备份集标识".to_string());
    }
    if backup_name.is_empty()
        || backup_name.contains('\\')
        || backup_name.contains('/')
        || backup_name.contains("..")
    {
        return Err("非法备份文件名".to_string());
    }
    let root = backups_root_for(sessions_dir);
    let set_dir = root.join(set_dir_id);
    let manifest = read_manifest(&set_dir)
        .ok_or_else(|| "备份清单不存在（旧格式集合不支持按会话回滚）".to_string())?;
    let entry = manifest
        .files
        .iter()
        .find(|f| f.backup_name == backup_name && f.source_path == source_path)
        .ok_or_else(|| "备份记录不存在".to_string())?;
    let bytes = fs::read(set_dir.join(&entry.backup_name)).map_err(|e| format!("备份文件缺失: {e}"))?;
    if sha256_hex(&bytes) != entry.sha256 {
        return Err("备份 sha256 校验不符（备份可能损坏），已拒绝还原".to_string());
    }
    // 先快照当前状态（回滚本身可撤销）
    let pre_dir = root.join(set_dir_name());
    let mut pre_restore_dir = String::new();
    if let Ok(cur) = fs::read(source_path) {
        let _ = fs::create_dir_all(&pre_dir);
        if fs::write(pre_dir.join(&entry.backup_name), &cur).is_ok() {
            write_manifest(
                &pre_dir,
                &BackupManifest {
                    schema_version: 1,
                    created_at: now_millis(),
                    trigger: "pre-restore".to_string(),
                    target_version: manifest.target_version.clone(),
                    files: vec![BackupFileEntry {
                        source_path: source_path.to_string(),
                        backup_name: entry.backup_name.clone(),
                        sha256: sha256_hex(&cur),
                        bytes: cur.len() as u64,
                        unknown_types: vec![],
                    }],
                },
            )?;
            pre_restore_dir = pre_dir.to_string_lossy().to_string();
        }
    }
    write_atomic(Path::new(source_path), &bytes)?;
    Ok(RestoreOneResult { pre_restore_dir })
}

// ==================== 截断回滚（破坏性兜底） ====================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncateOutcome {
    pub path: String,
    /// truncated | skipped | failed
    pub status: String,
    pub removed_count: u64,
    /// 被删首个事件的 seq（解析不出为 null）
    pub removed_from_seq: Option<i64>,
    pub removed_types: Vec<String>,
    pub reason: String,
    pub verified: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncateResult {
    pub backup_dir: String,
    pub outcomes: Vec<TruncateOutcome>,
}

/// 截断单个文件：删除第一个未知未标事件及其后全部内容。
/// 破坏性定义——后续对话一并丢弃；仅作为修复无效时的显式兜底。
fn truncate_one(
    path: &Path,
    backup_dir: &Path,
    known: &HashSet<String>,
    active_window: Duration,
) -> (TruncateOutcome, Option<BackupFileEntry>) {
    let pstr = path.to_string_lossy().to_string();
    match truncate_one_inner(path, backup_dir, known, active_window) {
        Ok(r) => r,
        Err(e) => (
            TruncateOutcome {
                path: pstr,
                status: "failed".to_string(),
                removed_count: 0,
                removed_from_seq: None,
                removed_types: vec![],
                reason: e,
                verified: false,
            },
            None,
        ),
    }
}

fn truncate_one_inner(
    path: &Path,
    backup_dir: &Path,
    known: &HashSet<String>,
    active_window: Duration,
) -> Result<(TruncateOutcome, Option<BackupFileEntry>), String> {
    let pstr = path.to_string_lossy().to_string();
    let outcome = |status: &str, removed: u64, seq: Option<i64>, types: Vec<String>, reason: &str, verified: bool| TruncateOutcome {
        path: pstr.clone(),
        status: status.to_string(),
        removed_count: removed,
        removed_from_seq: seq,
        removed_types: types,
        reason: reason.to_string(),
        verified,
    };
    if let Some(m) = fs::metadata(path).ok().and_then(|m| m.modified().ok()) {
        let recent = SystemTime::now().duration_since(m).unwrap_or(Duration::ZERO);
        if recent < active_window {
            return Ok((
                outcome("skipped", 0, None, vec![], "会话近期仍在写入，请先关闭该会话再截断", false),
                None,
            ));
        }
    }
    let buf = fs::read(path).map_err(|e| format!("读取失败: {e}"))?;
    let log = decode_session_log(&buf).map_err(|e| format!("解码失败: {e}"))?;
    // 找第一个未知未标事件（帧号, 行号, seq）
    let mut cut: Option<(usize, usize, i64)> = None;
    'outer: for (fi, fls) in log.frame_lines.iter().enumerate() {
        for (li, l) in fls.iter().enumerate() {
            let Some(t) = line_type(l) else { continue };
            if fi == 0 && li == 0 && t == "session" {
                continue;
            }
            if known.contains(&t) || line_ignorable(l) {
                continue;
            }
            let seq = serde_json::from_str::<serde_json::Value>(l.trim())
                .ok()
                .and_then(|v| v.get("seq").and_then(|x| x.as_i64()));
            cut = Some((fi, li, seq.unwrap_or(-1)));
            break 'outer;
        }
    }
    let Some((cf, cl, seq)) = cut else {
        return Ok((outcome("skipped", 0, None, vec![], "没有需要截断的未知事件", true), None));
    };
    // cut 及之后的全部行都被丢弃（含已知/已标事件——它们在断点之后）
    let mut removed_count = 0u64;
    let mut removed_types: Vec<String> = Vec::new();
    for (fi, fls) in log.frame_lines.iter().enumerate().skip(cf) {
        let start = if fi == cf { cl } else { 0 };
        removed_count += (fls.len() - start) as u64;
        for l in &fls[start..] {
            if let Some(t) = line_type(l) {
                if !removed_types.contains(&t) {
                    removed_types.push(t);
                }
            }
        }
    }
    let entry = backup_bytes(backup_dir, path, &buf, removed_types.clone())?;
    // 重建：cut 前整帧原字节拷贝；cut 所在帧保留前缀重压；其后帧与断尾丢弃。
    // 保留的事件保持原帧边界与校验和。
    let rebuild = (|| -> Result<Vec<u8>, String> {
        let mut rebuilt = Vec::with_capacity(buf.len());
        for (fi, (s, e)) in log.frames.iter().enumerate() {
            if fi < cf {
                rebuilt.extend_from_slice(&buf[*s..*e]);
            } else if fi == cf {
                let kept = &log.frame_lines[fi][..cl];
                let text = rebuild_frame_text(kept, log.frame_trailing_nl[fi]);
                rebuilt.extend_from_slice(&compress_frame(text.as_bytes())?);
            }
        }
        Ok(rebuilt)
    })();
    match rebuild.and_then(|r| write_atomic(path, &r)) {
        Ok(()) => {
            let verified = fs::read(path)
                .map_err(|e| format!("复查读取失败: {e}"))
                .and_then(|b| decode_session_log(&b))
                .map(|log2| count_blocking(&log2, known) == 0)
                .unwrap_or(false);
            Ok((outcome("truncated", removed_count, Some(seq), removed_types, "", verified), Some(entry)))
        }
        Err(e) => Ok((outcome("failed", 0, None, vec![], &e, false), Some(entry))),
    }
}

/// 批量截断（支持单个）
pub fn truncate_sessions(
    paths: &[PathBuf],
    backups_root: &Path,
    known: &HashSet<String>,
    target_version: &str,
    active_window: Duration,
) -> Result<TruncateResult, String> {
    if paths.is_empty() {
        return Err("未选择要截断的会话".to_string());
    }
    let backup_dir = backups_root.join(set_dir_name());
    let mut outcomes = Vec::new();
    let mut entries = Vec::new();
    for p in paths {
        let (o, e) = truncate_one(p, &backup_dir, known, active_window);
        outcomes.push(o);
        if let Some(e) = e {
            entries.push(e);
        }
    }
    if !entries.is_empty() {
        write_manifest(
            &backup_dir,
            &BackupManifest {
                schema_version: 1,
                created_at: now_millis(),
                trigger: "pre-truncate".to_string(),
                target_version: target_version.to_string(),
                files: entries,
            },
        )?;
    }
    Ok(TruncateResult {
        backup_dir: backup_dir.to_string_lossy().to_string(),
        outcomes,
    })
}

// ==================== 其他 ====================

/// 流式解压一个会话文件为文本行（测试与内容检查用；多帧流式解码器天然支持）
pub fn read_session_lines(path: &Path) -> Result<Vec<String>, String> {
    let f = fs::File::open(path).map_err(|e| format!("打开失败: {e}"))?;
    let mut dec = zstd::stream::read::Decoder::new(f).map_err(|e| format!("zstd 解压失败: {e}"))?;
    let mut data = Vec::new();
    dec.read_to_end(&mut data).map_err(|e| format!("读取解压流失败: {e}"))?;
    Ok(String::from_utf8_lossy(&data)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// 默认会话目录（env.home_dir + /sessions）
pub fn sessions_dir_for_home(home: &Path) -> PathBuf {
    home.join("sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dshpm-sfix-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_session(path: &Path, events: &[serde_json::Value]) {
        let plain: String = events
            .iter()
            .map(|v| serde_json::to_string(v).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let compressed = zstd::stream::encode_all(plain.as_bytes(), 3).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, compressed).unwrap();
    }

    fn known_set() -> HashSet<String> {
        ["user/message", "assistant/message", "tool/call"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    fn ev(o: serde_json::Value) -> serde_json::Value {
        o
    }

    fn fixture_events() -> Vec<serde_json::Value> {
        vec![
            ev(serde_json::json!({"type": "session", "version": 4, "id": "s1"})),
            ev(serde_json::json!({"type": "user/message", "seq": 1, "text": "你好"})),
            ev(serde_json::json!({"type": "job/registered", "seq": 2, "job": {"id": "j1"}})),
            ev(serde_json::json!({"type": "assistant/message", "seq": 3, "text": "回"})),
            ev(serde_json::json!({"type": "future/unknown", "seq": 4, "ignorable": true})),
        ]
    }

    #[test]
    fn test_scan_detects_faulty_session() {
        let d = tmpdir("scan");
        let ws = d.join("--E-workspace-x--").join("session-abc");
        write_session(&ws.join("session.v4.jsonl.zstd"), &fixture_events());
        let items = scan_sessions_dir(&d, &known_set()).unwrap();
        assert_eq!(items.len(), 1);
        let it = &items[0];
        assert!(it.faulty && it.readable);
        assert!(!it.torn_tail);
        let _ = it.active;
        assert_eq!(it.session_id, "session-abc");
        assert_eq!(it.workspace, "--E-workspace-x--");
        let kinds: Vec<&str> = it.unknown.iter().map(|u| u.kind.as_str()).collect();
        assert_eq!(kinds, vec!["job/registered"], "已标 ignorable 的不算故障");
        assert_eq!(it.unknown[0].count, 1);
        assert_eq!(it.unknown[0].seqs, vec![2]);
    }

    #[test]
    fn test_scan_item_metadata() {
        let d = tmpdir("meta");
        let ws = d.join("ws");
        let events = vec![
            serde_json::json!({"type": "session", "version": 4, "id": "s-md"}),
            serde_json::json!({"type": "user/message", "seq": 1, "data": {"message": {"content": [{"type": "text", "text": "第一条消息"}]}}}),
            serde_json::json!({"type": "session/title", "seq": 2, "data": {"title": "我的会话标题"}}),
            serde_json::json!({"type": "job/registered", "seq": 9}),
        ];
        write_session(&ws.join("session-md").join("session.v4.jsonl.zstd"), &events);
        let items = scan_sessions_dir(&d, &known_set()).unwrap();
        let it = items.iter().find(|i| i.session_id == "session-md").unwrap();
        assert_eq!(it.title, "我的会话标题", "session/title 优先");
        assert_eq!(it.format_version, Some(4));
        assert_eq!(it.max_seq, 9);
        assert_eq!(it.unknown[0].kind, "job/registered");
        assert_eq!(it.unknown[0].seqs, vec![9]);

        // 无标题 → 兜底首条 user/message 文本
        let events2 = vec![
            serde_json::json!({"type": "session", "version": 4, "id": "s-md2"}),
            serde_json::json!({"type": "user/message", "seq": 1, "data": {"message": {"content": [{"type": "text", "text": "兜底标题"}]}}}),
        ];
        write_session(&ws.join("session-md2").join("session.v4.jsonl.zstd"), &events2);
        let items2 = scan_sessions_dir(&d, &known_set()).unwrap();
        let it2 = items2.iter().find(|i| i.session_id == "session-md2").unwrap();
        assert_eq!(it2.title, "兜底标题");
        assert_eq!(it2.unknown.len(), 0);
        assert_eq!(it2.max_seq, 1);
        assert!(!it2.faulty);
    }

    #[test]
    fn test_backup_list_and_restore_one() {
        let d = tmpdir("bklist");
        let data = d.join("data");
        let f = data.join("ws").join("session-bk").join("session.v4.jsonl.zstd");
        write_session(&f, &fixture_events());
        let original = fs::read(&f).unwrap();
        let backups = backups_root_for(&data);
        // 第一次修复 → pre-repair 备份
        fix_sessions(&[f.clone()], &backups, &known_set(), "0.2.0", Duration::ZERO).unwrap();
        let b1 = list_session_backups(&data, &f.to_string_lossy()).unwrap();
        assert_eq!(b1.len(), 1);
        assert_eq!(b1[0].trigger, "pre-repair");
        assert_eq!(b1[0].bytes, original.len() as u64);
        // 再弄坏 → 第二次修复 → 第二条备份
        write_session(&f, &fixture_events());
        fix_sessions(&[f.clone()], &backups, &known_set(), "0.2.0", Duration::ZERO).unwrap();
        let b2 = list_session_backups(&data, &f.to_string_lossy()).unwrap();
        assert_eq!(b2.len(), 2, "两条备份");
        // 回滚到最新备份 = 修复前字节；当前状态先快照为 pre-restore
        let r = restore_session_backup(&data, &b2[0].set_dir, &b2[0].backup_name, &f.to_string_lossy()).unwrap();
        assert!(!r.pre_restore_dir.is_empty());
        assert_eq!(fs::read(&f).unwrap(), original, "按备份回滚应恢复修复前字节");
        let b3 = list_session_backups(&data, &f.to_string_lossy()).unwrap();
        assert_eq!(b3[0].trigger, "pre-restore", "回滚产生的新备份排最前");
        // 非法集合名 / 备份名拒绝
        assert!(restore_session_backup(&data, "../x", "y", &f.to_string_lossy()).is_err());
        assert!(restore_session_backup(&data, "999999", "a/b", &f.to_string_lossy()).is_err());
    }

    #[test]
    fn test_fix_adds_ignorable_and_is_idempotent() {
        let d = tmpdir("fix");
        // 会话树放进 data 子目录：backups_root_for 的备份根随之隔离（不与其他测试共享）
        let data = d.join("data");
        let f = data.join("ws").join("session-x").join("session.v4.jsonl.zstd");
        write_session(&f, &fixture_events());
        let backups = backups_root_for(&data);
        let res = fix_sessions(&[f.clone()], &backups, &known_set(), "0.2.0", Duration::ZERO).unwrap();
        assert_eq!(res.outcomes.len(), 1);
        assert_eq!(res.outcomes[0].status, "fixed");
        assert_eq!(res.outcomes[0].fixed_lines, 1, "只补 job/registered 一行");
        assert!(res.outcomes[0].verified, "修复后复查应通过");
        // 备份 + 清单
        let bdir = PathBuf::from(&res.backup_dir);
        assert!(bdir.join("session-x-session.v4.jsonl.zstd").exists(), "有备份");
        let m: BackupManifest =
            serde_json::from_str(&fs::read_to_string(bdir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(m.trigger, "pre-repair");
        assert_eq!(m.files.len(), 1);

        // 内容校验：其他字段原样（含 seq），新字段在末尾
        let lines = read_session_lines(&f).unwrap();
        let job: serde_json::Value =
            serde_json::from_str(&lines.iter().find(|l| l.contains("job/registered")).unwrap()).unwrap();
        assert_eq!(job["ignorable"], serde_json::json!(true));
        assert_eq!(job["seq"], serde_json::json!(2));
        assert_eq!(job["job"]["id"], serde_json::json!("j1"));
        // 原始行字节序保留（在 user/message 与 assistant/message 之间的行仍在其间）
        let job_pos = lines.iter().position(|l| l.contains("job/registered")).unwrap();
        let user_pos = lines.iter().position(|l| l.contains("user/message")).unwrap();
        let asst_pos = lines.iter().position(|l| l.contains("assistant/message")).unwrap();
        assert!(user_pos < job_pos && job_pos < asst_pos);

        // 修复后再扫 → 无故障；再修 → 幂等零写入
        let items = scan_sessions_dir(&data, &known_set()).unwrap();
        assert!(items.iter().all(|i| !i.faulty), "修复后应无故障会话");
        let res2 = fix_sessions(&[f.clone()], &backups, &known_set(), "0.2.0", Duration::ZERO).unwrap();
        assert_eq!(res2.outcomes[0].status, "unchanged");
        assert_eq!(res2.outcomes[0].fixed_lines, 0);
    }

    #[test]
    fn test_fix_single_session_only_touches_selected() {
        let d = tmpdir("single");
        let data = d.join("data");
        let a = data.join("ws1").join("session-a").join("session.v4.jsonl.zstd");
        let b = data.join("ws2").join("session-b").join("session.v4.jsonl.zstd");
        write_session(&a, &fixture_events());
        write_session(&b, &fixture_events());
        let backups = backups_root_for(&data);
        let res = fix_sessions(&[a.clone()], &backups, &known_set(), "0.2.0", Duration::ZERO).unwrap();
        assert_eq!(res.outcomes.len(), 1);
        // b 未被触碰：仍为故障态
        let items = scan_sessions_dir(&data, &known_set()).unwrap();
        let b_item = items.iter().find(|i| i.path == b.to_string_lossy()).unwrap();
        assert!(b_item.faulty);
    }

    #[test]
    fn test_rollback_restores_original() {
        let d = tmpdir("rb");
        let data = d.join("data");
        let f = data.join("ws").join("session-r").join("session.v4.jsonl.zstd");
        write_session(&f, &fixture_events());
        let before = fs::read(&f).unwrap();
        let backups = backups_root_for(&data);
        fix_sessions(&[f.clone()], &backups, &known_set(), "0.2.0", Duration::ZERO).unwrap();
        let after = fs::read(&f).unwrap();
        assert_ne!(before, after, "修复应改变文件");
        let rb = rollback_last(&data).unwrap();
        assert_eq!(rb.restored, 1);
        assert!(rb.failed.is_empty());
        assert!(!rb.pre_restore_dir.is_empty(), "回滚前应有 pre-restore 快照");
        assert_eq!(fs::read(&f).unwrap(), before, "回滚应恢复原始字节");
    }

    #[test]
    fn test_fix_line_variants() {
        let known = known_set();
        // 已知类型不动
        assert_eq!(fix_line(r#"{"type":"user/message","seq":1}"#, &known), None);
        // 未知 + 已标 true → 幂等不动
        assert_eq!(
            fix_line(r#"{"type":"x/y","ignorable":true}"#, &known),
            None
        );
        // 未知 + 未标 → 末尾插入
        assert_eq!(
            fix_line(r#"{"type":"x/y","seq":2}"#, &known),
            Some(r#"{"type":"x/y","seq":2,"ignorable":true}"#.to_string())
        );
        // 未知 + false → serde 重写为 true
        assert_eq!(
            fix_line(r#"{"type":"x/y","ignorable":false}"#, &known),
            Some(r#"{"ignorable":true,"type":"x/y"}"#.to_string())
        );
        // 非 JSON / 非对象 → 不动
        assert_eq!(fix_line("not json", &known), None);
        assert_eq!(fix_line("[1,2]", &known), None);
    }

    #[test]
    fn test_parse_known_types_extracts_set() {
        let d = tmpdir("vocab");
        let js = d.join("known-event-types.js");
        fs::write(
            &js,
            "export const KNOWN_SESSION_EVENT_TYPES = new Set([\n  'session',\n  'user/message',\n  'tool/call',\n]);\n",
        )
        .unwrap();
        let types = parse_known_types(&js).unwrap();
        assert_eq!(types, vec!["session", "user/message", "tool/call"]);
    }

    #[test]
    fn test_pick_vocab_fallback() {
        let v = |ver: &str| VocabEntry {
            info: VocabInfo { version: ver.into(), count: 1, source: String::new(), builtin: false },
            types: vec![],
        };
        let vocabs = vec![v("0.1.0"), v("0.2.0"), v("0.2.1-rc.2")];
        // 精确命中
        assert_eq!(pick_vocab_entry(&vocabs, "0.2.0").unwrap().info.version, "0.2.0");
        // 目标更高 → 最近更低 = 0.2.1-rc.2
        assert_eq!(pick_vocab_entry(&vocabs, "0.3.0").unwrap().info.version, "0.2.1-rc.2");
        // 目标更低且无更低词汇表 → 用最旧的
        assert_eq!(pick_vocab_entry(&vocabs, "0.0.9").unwrap().info.version, "0.1.0");
        // 介于之间 → 最近更低
        assert_eq!(pick_vocab_entry(&vocabs, "0.2.1").unwrap().info.version, "0.2.1-rc.2");
        // rc 预发布序：0.2.1-rc.1 → 低于 0.2.1-rc.2
        assert_eq!(pick_vocab_entry(&vocabs, "0.2.1-rc.1").unwrap().info.version, "0.2.0");
        // release 高于同核 rc
        assert_eq!(pick_vocab_entry(&vocabs, "0.1.0-rc.1").unwrap().info.version, "0.1.0");
    }

    #[test]
    fn test_builtin_vocabs_and_merge() {
        let b = builtin_vocabs();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].info.version, "0.2.0");
        assert_eq!(b[0].types.len(), 59, "0.2.0 内置 59 种");
        assert_eq!(b[1].info.version, "0.2.1");
        assert_eq!(b[1].types.len(), 62, "0.2.1 内置 62 种");
        assert!(b[1].types.contains(&"job/registered".to_string()));
        assert!(!b[0].types.contains(&"job/registered".to_string()));
        assert!(b.iter().all(|e| e.info.builtin));

        // 合并：探测到同版本 → 内置优先不被覆盖；探测到新版本 → 追加
        let d = tmpdir("merge");
        let pkg = d.join("dsh-x").join("node_modules").join("@deepseek-ai").join("dsh-session");
        fs::create_dir_all(pkg.join("lib").join("types")).unwrap();
        fs::write(pkg.join("package.json"), "{\"version\":\"0.2.0\"}").unwrap();
        fs::write(
            pkg.join("lib").join("types").join("known-event-types.js"),
            "export const KNOWN_SESSION_EVENT_TYPES = new Set([\n  'user/message',\n]);\n",
        )
        .unwrap();
        let m = merged_vocabs(&[d.clone()]);
        let v020 = m.iter().find(|e| e.info.version == "0.2.0").unwrap();
        assert!(v020.info.builtin, "同版本内置优先");
        assert_eq!(v020.types.len(), 59, "不被探测的同版本覆盖");

        // 换成新版本号 → 追加为非内置条目
        fs::write(pkg.join("package.json"), "{\"version\":\"3.0.0\"}").unwrap();
        let m2 = merged_vocabs(&[d]);
        let v300 = m2.iter().find(|e| e.info.version == "3.0.0").unwrap();
        assert!(!v300.info.builtin);
        assert_eq!(v300.types.len(), 1);
        assert!(v300.info.source.contains("known-event-types.js"));
        // 升序
        let vers: Vec<&str> = m2.iter().map(|e| e.info.version.as_str()).collect();
        let mut sorted = vers.clone();
        sorted.sort_by_key(|v| version_key(v));
        assert_eq!(vers, sorted);
    }

    #[test]
    fn test_frame_scan_multi_and_torn() {
        let f1 = compress_frame(
            b"{\"type\":\"session\",\"id\":\"h\"}\n{\"type\":\"user/message\",\"seq\":1}\n",
        )
        .unwrap();
        let f2 = compress_frame(b"{\"type\":\"assistant/message\",\"seq\":2}\n").unwrap();
        let mut buf = f1.clone();
        buf.extend_from_slice(&f2);
        let scan = scan_zstd_frames(&buf).unwrap();
        assert_eq!(scan.frames.len(), 2);
        assert!(scan.torn_start.is_none());
        let log = decode_session_log(&buf).unwrap();
        assert_eq!(log.frame_lines.len(), 2);
        assert_eq!(log.frame_lines[0].len(), 2);
        assert_eq!(log.frame_lines[1].len(), 1);

        // 断尾：剪掉最后 3 字节 → 第二帧不完整，第一帧仍完整
        let torn = &buf[..buf.len() - 3];
        let scan2 = scan_zstd_frames(torn).unwrap();
        assert_eq!(scan2.frames.len(), 1);
        assert!(scan2.torn_start.is_some());
        let log2 = decode_session_log(torn).unwrap();
        assert_eq!(log2.frame_lines.len(), 1, "完整帧仍可解");
        assert!(log2.torn_start.is_some());
    }

    #[test]
    fn test_fix_preserves_unchanged_frame_bytes() {
        let d = tmpdir("frames");
        let data = d.join("data");
        let f = data.join("ws").join("session-fr").join("session.v4.jsonl.zstd");
        let frame1_text =
            "{\"type\":\"session\",\"id\":\"h\"}\n{\"type\":\"user/message\",\"seq\":1}\n";
        let frame2_text = "{\"type\":\"job/registered\",\"seq\":2}\n";
        let mut raw = compress_frame(frame1_text.as_bytes()).unwrap();
        raw.extend_from_slice(&compress_frame(frame2_text.as_bytes()).unwrap());
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(&f, &raw).unwrap();
        let backups = backups_root_for(&data);
        let res = fix_sessions(&[f.clone()], &backups, &known_set(), "0.2.1", Duration::ZERO).unwrap();
        assert_eq!(res.outcomes[0].status, "fixed");
        assert_eq!(res.outcomes[0].fixed_lines, 1);
        assert!(res.outcomes[0].verified);

        // 未动帧逐字节保留、帧布局保留
        let after = fs::read(&f).unwrap();
        let scan = scan_zstd_frames(&after).unwrap();
        assert_eq!(scan.frames.len(), 2, "帧布局应保留");
        let (s0, e0) = scan.frames[0];
        assert_eq!(&after[s0..e0], &raw[..e0], "帧 1 应原字节拷贝（校验和保持有效）");
        // 被改帧可解出修复内容
        let log = decode_session_log(&after).unwrap();
        assert_eq!(
            log.frame_lines[1][0],
            "{\"type\":\"job/registered\",\"seq\":2,\"ignorable\":true}"
        );
    }

    #[test]
    fn test_fix_skips_recently_active() {
        let d = tmpdir("active");
        let data = d.join("data");
        let f = data.join("ws").join("session-a2").join("session.v4.jsonl.zstd");
        write_session(&f, &fixture_events());
        let backups = backups_root_for(&data);
        // 刚写完 → 90 秒窗口内 → 跳过
        let res = fix_sessions(
            &[f.clone()],
            &backups,
            &known_set(),
            "0.2.0",
            Duration::from_secs(ACTIVE_WINDOW_SECS),
        )
        .unwrap();
        assert_eq!(res.outcomes[0].status, "skipped");
        assert!(res.outcomes[0].reason.contains("写入"));
        // 零窗口（调用方可传）则正常修复
        let res2 = fix_sessions(&[f.clone()], &backups, &known_set(), "0.2.0", Duration::ZERO).unwrap();
        assert_eq!(res2.outcomes[0].status, "fixed");
    }

    #[test]
    fn test_truncate_cuts_at_first_unknown() {
        let d = tmpdir("trunc");
        let data = d.join("data");
        let f = data.join("ws").join("session-t").join("session.v4.jsonl.zstd");
        let mut events = fixture_events();
        events.push(serde_json::json!({"type": "user/message", "seq": 5, "text": "之后的话"}));
        write_session(&f, &events);
        let backups = backups_root_for(&data);
        let res = truncate_sessions(&[f.clone()], &backups, &known_set(), "0.2.0", Duration::ZERO).unwrap();
        let o = &res.outcomes[0];
        assert_eq!(o.status, "truncated");
        assert_eq!(o.removed_count, 4, "job/2 + asst/3 + future/4 + user/5");
        assert_eq!(o.removed_from_seq, Some(2));
        assert!(o.verified);
        assert!(o.removed_types.contains(&"job/registered".to_string()));

        let lines = read_session_lines(&f).unwrap();
        assert_eq!(lines.len(), 2, "头 + user/1");
        assert!(lines[1].contains("user/message"));
        // 备份在案
        let m: BackupManifest =
            serde_json::from_str(&fs::read_to_string(Path::new(&res.backup_dir).join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(m.trigger, "pre-truncate");
        assert_eq!(m.files.len(), 1);
        // 幂等：再截 → skipped
        let res2 = truncate_sessions(&[f.clone()], &backups, &known_set(), "0.2.0", Duration::ZERO).unwrap();
        assert_eq!(res2.outcomes[0].status, "skipped");
    }

    #[test]
    fn test_truncate_clean_session_skips() {
        let d = tmpdir("trunc2");
        let data = d.join("data");
        let f = data.join("ws").join("session-t2").join("session.v4.jsonl.zstd");
        write_session(
            &f,
            &[
                serde_json::json!({"type": "session", "id": "x"}),
                serde_json::json!({"type": "user/message", "seq": 1}),
            ],
        );
        let res = truncate_sessions(&[f.clone()], &backups_root_for(&data), &known_set(), "0.2.0", Duration::ZERO)
            .unwrap();
        assert_eq!(res.outcomes[0].status, "skipped");
    }

    #[test]
    fn test_rollback_legacy_name_matched_set() {
        let d = tmpdir("legacy");
        let data = d.join("data");
        let f = data.join("ws").join("session-lg").join("session.v4.jsonl.zstd");
        write_session(&f, &fixture_events());
        let backups = backups_root_for(&data);
        let original = fs::read(&f).unwrap();
        // 手工构造旧格式备份集（按文件名平铺、无 manifest）
        let set_dir = backups.join("1700000000000");
        fs::create_dir_all(&set_dir).unwrap();
        fs::copy(&f, set_dir.join("session.v4.jsonl.zstd")).unwrap();
        // 模拟修复后的变化
        fs::write(&f, zstd::stream::encode_all(&b"{\"type\":\"x\"}\n"[..], 3).unwrap()).unwrap();
        let rb = rollback_last(&data).unwrap();
        assert_eq!(rb.restored, 1);
        assert!(rb.pre_restore_dir.is_empty(), "legacy 路径无 pre-restore 快照");
        assert_eq!(fs::read(&f).unwrap(), original);
    }

    #[test]
    fn test_session_header_exempt_but_midlog_faulty() {
        let d = tmpdir("header");
        let known = known_set();
        // 首行 session 头（不在词汇表）→ 不算故障
        let events = fixture_events();
        let f = d.join("ws").join("session-h").join("session.v4.jsonl.zstd");
        write_session(&f, &events);
        let items = scan_sessions_dir(&d, &known).unwrap();
        assert_eq!(items.len(), 1);
        let kinds: Vec<&str> = items[0].unknown.iter().map(|u| u.kind.as_str()).collect();
        assert_eq!(kinds, vec!["job/registered"], "session 头应被豁免");
        // 日志中间出现 session 行 → 真故障
        let events2 = fixture_events();
        let mut events_mid = vec![events2[0].clone(), events2[1].clone()];
        events_mid.push(serde_json::json!({"type": "session", "seq": 9}));
        events_mid.push(events2[2].clone());
        let f2 = d.join("ws").join("session-mid").join("session.v4.jsonl.zstd");
        write_session(&f2, &events_mid);
        let items2 = scan_sessions_dir(&d, &known).unwrap();
        let mid = items2.iter().find(|i| i.session_id == "session-mid").unwrap();
        assert!(
            mid.unknown.iter().any(|u| u.kind == "session"),
            "日志中间的 session 行应计为未知事件"
        );
    }

    #[test]
    fn test_is_session_log_name() {
        assert!(is_session_log_name("session.v4.jsonl.zstd"));
        assert!(is_session_log_name("session.v5.jsonl.zstd"));
        assert!(is_session_log_name("session.jsonl.zstd"));
        assert!(!is_session_log_name("other.jsonl.zstd"));
        assert!(!is_session_log_name("session.v4.jsonl"));
        assert!(!is_session_log_name("readme.md"));
    }

    /// 探针：真实根目录下词汇表探测（复现命令层逻辑）
    #[test]
    #[ignore]
    fn test_probe_real_vocab_roots() {
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join(".dsh"));
            roots.push(home.join(".dsh").join("profiles"));
        }
        let settings_file = dirs::data_dir().unwrap().join("dsh-plugin-manager").join("settings.json");
        if let Ok(text) = std::fs::read_to_string(&settings_file) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(dl) = v.get("dshDownloadDir").and_then(|x| x.as_str()) {
                    eprintln!("settings.dshDownloadDir = {dl:?}");
                    roots.push(PathBuf::from(dl));
                }
            }
        }
        eprintln!("roots = {:?}", roots);
        for r in &roots {
            let mut files = Vec::new();
            find_vocab_files(r, &mut files);
            eprintln!("root {:?} → vocab files: {}", r, files.len());
            for f in &files {
                eprintln!("   {:?}", f);
            }
        }
        let entries = merged_vocabs(&roots);
        eprintln!("merged_vocabs → {} 个版本", entries.len());
        for e in &entries {
            eprintln!(
                "   {} ({} 种, builtin={}, src={})",
                e.info.version, e.info.count, e.info.builtin, e.info.source
            );
        }
    }

    /// 真机黄金验证（只读）：扫描真实 ~/.dsh/sessions（帧级解码引擎真实文件）
    /// `cargo test --lib session_fix::tests::test_scan_real_sessions_golden -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn test_scan_real_sessions_golden() {
        let Some(home) = dirs::home_dir() else { return };
        let dir = home.join(".dsh").join("sessions");
        if !dir.is_dir() {
            eprintln!("SKIP: 无真实会话目录");
            return;
        }
        let js = PathBuf::from(r"C:/dsh-versions/dsh-0.2.1-rc.2/node_modules/@deepseek-ai/dsh-session/lib/types/known-event-types.js");
        if !js.is_file() {
            eprintln!("SKIP: 本机无 0.2.1-rc.2 词汇表");
            return;
        }
        let known: HashSet<String> = parse_known_types(&js).unwrap().into_iter().collect();
        eprintln!("词汇表: {} 种", known.len());
        let items = scan_sessions_dir(&dir, &known).unwrap();
        let unreadable = items.iter().filter(|i| !i.readable).count();
        let torn = items.iter().filter(|i| i.torn_tail).count();
        let active = items.iter().filter(|i| i.active).count();
        let faulty: Vec<&SessionScanItem> = items.iter().filter(|i| i.faulty).collect();
        eprintln!(
            "会话: {} 个 | 不可读: {unreadable} | 断尾: {torn} | 活跃: {active} | 故障: {}",
            items.len(),
            faulty.len()
        );
        for i in faulty.iter().take(5) {
            let kinds: Vec<String> = i.unknown.iter().map(|u| format!("{}×{}", u.kind, u.count)).collect();
            eprintln!("  故障: {} | {} 行 | {}", i.session_id, i.lines, kinds.join(", "));
        }
        assert_eq!(unreadable, 0, "真实会话应全部可读（帧级解码）");
        assert!(items.len() >= 50, "应扫描到大量真实会话");
    }
}
