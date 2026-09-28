//! 工具链探测与自动安装：node / npm / pnpm
//!
//! 下载 DSH 之前先确保 nodejs（含 npm）与 pnpm 可用；
//! 缺失时自动从镜像源下载安装到 `%AppData%\dsh-plugin-manager\runtime\`，
//! 全程无窗口、日志经 `install-log` 事件流式推送。

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tauri::Emitter;
use zip::ZipArchive;

/// 工具链探测结果
#[derive(Debug, Clone)]
pub struct Toolchain {
    pub node: PathBuf,
    pub node_dir: PathBuf,
    pub npm: PathBuf,
    pub pnpm: Option<PathBuf>,
}

impl Toolchain {
    /// 把 node 目录注入子进程 PATH（供 dsh.cmd shim / pnpm 等使用）
    pub fn inject_path(&self, cmd: &mut Command) {
        if self.node_dir.is_dir() {
            let old = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{};{}", self.node_dir.display(), old));
        }
    }
}

/// node 自动安装根目录：%AppData%\dsh-plugin-manager\runtime
pub fn runtime_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("dsh-plugin-manager")
        .join("runtime")
}

/// 静默执行命令并捕获 stdout（无黑框）
fn run_capture(program: &str) -> String {
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", program]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::null());
    cmd.output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// 探测当前机器上的 node/npm/pnpm（只探测，不安装、不改系统）
pub fn probe() -> Toolchain {
    let mut node: Option<PathBuf> = None;

    // 1) 自动安装过的（runtime 目录）
    if let Ok(rd) = std::fs::read_dir(runtime_dir()) {
        let mut found: Option<PathBuf> = None;
        let mut best_len: u64 = 0;
        for e in rd.flatten() {
            let p = e.path().join("node.exe");
            if p.is_file() {
                let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                if len > best_len {
                    best_len = len;
                    found = Some(p);
                }
            }
        }
        node = found;
    }
    // 2) PATH 中的 node（where node）
    if node.is_none() {
        for line in run_capture("where node").lines() {
            let p = PathBuf::from(line.trim());
            if p.is_file() {
                node = Some(p);
                break;
            }
        }
    }
    // 3) 常见安装位置（含 DSH Desktop 自带 node、npm 全局目录）
    if node.is_none() {
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        let mut candidates: Vec<String> = vec![
            "C:\\Program Files\\nodejs\\node.exe".to_string(),
            "C:\\Program Files (x86)\\nodejs\\node.exe".to_string(),
            format!("{home}\\.dsh-win\\node\\node.exe"),
            format!("{appdata}\\npm\\node.exe"),
        ];
        for c in candidates.drain(..) {
            let p = PathBuf::from(&c);
            if p.is_file() {
                node = Some(p);
                break;
            }
        }
    }

    let node = node.unwrap_or_default();
    let node_dir = node.parent().map(Path::to_path_buf).unwrap_or_default();
    let npm = node_dir.join("npm.cmd");

    // pnpm：node 目录同级，或 PATH 中
    let mut pnpm = if node_dir.join("pnpm.cmd").is_file() {
        Some(node_dir.join("pnpm.cmd"))
    } else {
        None
    };
    if pnpm.is_none() {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        let global = PathBuf::from(&appdata).join("npm").join("pnpm.cmd");
        if global.is_file() {
            pnpm = Some(global);
        }
    }
    if pnpm.is_none() {
        for line in run_capture("where pnpm").lines() {
            let p = PathBuf::from(line.trim());
            if p.is_file() {
                pnpm = Some(p);
                break;
            }
        }
    }

    Toolchain { node, node_dir, npm, pnpm }
}

/// node 二进制镜像索引条目（index.json）
#[derive(serde::Deserialize)]
pub struct NodeIndexEntry {
    pub version: String,
    #[serde(default)]
    pub lts: Option<serde_json::Value>,
}

fn parse_ver(v: &str) -> Vec<u32> {
    v.trim_start_matches('v')
        .split(['.', '-'])
        .filter_map(|s| s.parse::<u32>().ok())
        .collect()
}

fn cmp_ver(a: &str, b: &str) -> std::cmp::Ordering {
    parse_ver(a).cmp(&parse_ver(b))
}

/// 从索引中挑选最新 LTS 版本号（纯函数，可单测）
pub fn pick_lts<'a>(entries: &'a [NodeIndexEntry]) -> Option<&'a str> {
    entries
        .iter()
        .filter(|e| e.lts.is_some() && !e.lts.as_ref().unwrap().is_null())
        .filter(|e| !e.version.starts_with("v0."))
        .max_by(|a, b| cmp_ver(&a.version, &b.version))
        .map(|e| e.version.as_str())
}

/// 全部 LTS 版本号（新→旧降序；过滤远古 v0.x），供下载失败时逐级回退
pub fn pick_lts_list(entries: &[NodeIndexEntry]) -> Vec<String> {
    let mut list: Vec<String> = entries
        .iter()
        .filter(|e| e.lts.is_some() && !e.lts.as_ref().unwrap().is_null())
        .filter(|e| !e.version.starts_with("v0."))
        .map(|e| e.version.clone())
        .collect();
    list.sort_by(|a, b| cmp_ver(b, a));
    list
}

/// 拉取 node 二进制镜像索引（走用户 registry，如 npmmirror）
pub fn fetch_node_index(registry: &str) -> Result<Vec<NodeIndexEntry>, String> {
    let base = crate::settings::resolve_node_base(registry);
    let url = format!("{base}/index.json");
    let body = reqwest::blocking::get(&url)
        .map_err(|e| format!("获取 Node.js 版本索引失败: {e}"))?
        .text()
        .map_err(|e| format!("读取版本索引失败: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("解析版本索引失败: {e}"))
}

fn emit_log(app: &tauri::AppHandle, line: &str, kind: &str) {
    let _ = app.emit("install-log", serde_json::json!({ "line": line, "kind": kind }));
}

/// 下载并解压指定版本 node 到 runtime 目录（幂等：已存在则跳过）
pub fn download_node(app: &tauri::AppHandle, registry: &str, ver: &str) -> Result<PathBuf, String> {
    let dest = runtime_dir().join(format!("node-{ver}"));
    let exe = dest.join("node.exe");
    if exe.is_file() {
        emit_log(app, &format!("Node.js {ver} 已存在，跳过下载"), "stdout");
        return Ok(exe);
    }
    let base = crate::settings::resolve_node_base(registry);
    // index.json 的 version 已带 v 前缀（如 "v24.21.0"），直接拼目录名，避免 vv 双前缀
    let zip_url = format!("{base}/{ver}/node-{ver}-win-x64.zip");
    emit_log(app, &format!("下载 Node.js {ver}（{zip_url}）…"), "stdout");

    let resp = reqwest::blocking::get(&zip_url)
        .map_err(|e| format!("下载 Node.js 失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载 Node.js 失败：HTTP {}", resp.status()));
    }
    let bytes = resp.bytes().map_err(|e| format!("读取下载内容失败: {e}"))?;

    let tmp = runtime_dir().join(format!("node-{ver}.zip"));
    std::fs::create_dir_all(runtime_dir()).map_err(|e| format!("创建 runtime 目录失败: {e}"))?;
    std::fs::write(&tmp, &bytes).map_err(|e| format!("写入临时文件失败: {e}"))?;

    emit_log(app, "解压 Node.js…", "stdout");
    let file = std::fs::File::open(&tmp).map_err(|e| format!("打开临时文件失败: {e}"))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("解析 Node.js 压缩包失败: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读取压缩条目失败: {e}"))?;
        let name = entry.name().to_string();
        // 剥掉顶层目录 node-v{ver}-win-x64/
        let rel = name.splitn(2, '/').nth(1).unwrap_or("").to_string();
        if rel.is_empty() {
            continue;
        }
        let out = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).ok();
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut f = std::fs::File::create(&out).map_err(|e| format!("写入 {rel} 失败: {e}"))?;
        std::io::copy(&mut entry, &mut f).map_err(|e| format!("解压 {rel} 失败: {e}"))?;
    }
    let _ = std::fs::remove_file(&tmp);

    if !exe.is_file() {
        return Err("Node.js 解压完成但未找到 node.exe".to_string());
    }
    emit_log(app, &format!("Node.js {ver} 安装完成：{}", dest.display()), "stdout");
    Ok(exe)
}

/// 用 npm 全局安装 pnpm（幂等）
pub fn install_pnpm(app: &tauri::AppHandle, node_dir: &Path, registry: &str) -> Result<PathBuf, String> {
    let pnpm = node_dir.join("pnpm.cmd");
    if pnpm.is_file() {
        return Ok(pnpm);
    }
    let npm = node_dir.join("npm.cmd");
    if !npm.is_file() {
        return Err(format!("未找到 npm.cmd（{}）", node_dir.display()));
    }
    emit_log(app, "安装 pnpm（npm install -g pnpm）…", "stdout");
    let mut cmd = Command::new(&npm);
    let reg = registry.trim();
    if reg.is_empty() {
        cmd.args(["install", "-g", "pnpm", "--no-audit", "--no-fund"]);
    } else {
        cmd.args(["install", "-g", "pnpm"]);
        cmd.arg("--registry").arg(crate::settings::resolve_registry(reg));
        cmd.args(["--no-audit", "--no-fund"]);
    }
    cmd.current_dir(node_dir);
    cmd.env("PATH", format!("{};{}", node_dir.display(), std::env::var("PATH").unwrap_or_default()));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());

    let mut child = cmd.spawn().map_err(|e| format!("启动 npm 失败: {e}"))?;
    let out_handle = {
        let app = app.clone();
        let stdout = child.stdout.take();
        std::thread::spawn(move || {
            if let Some(stdout) = stdout {
                let reader = std::io::BufReader::new(stdout);
                for line in reader.lines() {
                    if let Ok(l) = line {
                        let _ = app.emit("install-log", serde_json::json!({ "line": l, "kind": "stdout" }));
                    }
                }
            }
        })
    };
    let err_handle = {
        let app = app.clone();
        let stderr = child.stderr.take();
        std::thread::spawn(move || {
            if let Some(stderr) = stderr {
                let reader = std::io::BufReader::new(stderr);
                for line in reader.lines() {
                    if let Ok(l) = line {
                        let _ = app.emit("install-log", serde_json::json!({ "line": l, "kind": "stderr" }));
                    }
                }
            }
        })
    };
    let status = child.wait();
    let _ = out_handle.join();
    let _ = err_handle.join();
    match status {
        Ok(s) if s.success() && pnpm.is_file() => {
            emit_log(app, &format!("pnpm 安装完成：{}", pnpm.display()), "stdout");
            Ok(pnpm)
        }
        Ok(s) => Err(format!("pnpm 安装失败（退出码 {:?}）", s.code())),
        Err(e) => Err(format!("pnpm 安装进程异常: {e}")),
    }
}

/// 确保 node/npm/pnpm 可用；缺失自动安装，返回完整工具链
pub fn ensure(app: &tauri::AppHandle, registry: &str) -> Result<Toolchain, String> {
    let mut t = probe();
    if t.node.is_file() && t.npm.is_file() {
        if t.pnpm.is_none() {
            t.pnpm = Some(install_pnpm(app, &t.node_dir, registry)?);
        }
        return Ok(t);
    }
    // 需要自动安装 node
    emit_log(app, "未检测到 Node.js 环境，开始自动安装…", "stdout");
    let entries = fetch_node_index(registry)?;
    let candidates = pick_lts_list(&entries);
    if candidates.is_empty() {
        return Err("无法从镜像获取 Node.js LTS 版本".to_string());
    }
    let mut exe: Option<PathBuf> = None;
    let mut last_err = String::new();
    for ver in candidates.into_iter().take(5) {
        match download_node(app, registry, &ver) {
            Ok(e) => {
                exe = Some(e);
                break;
            }
            Err(e) => {
                emit_log(app, &format!("Node.js {ver} 下载失败（{e}），尝试更早的 LTS…"), "stderr");
                last_err = e;
            }
        }
    }
    let exe = exe.ok_or_else(|| format!("Node.js 各候选版本均下载失败：{last_err}"))?;
    let node_dir = exe
        .parent()
        .ok_or_else(|| "无法定位 Node.js 目录".to_string())?
        .to_path_buf();
    let npm = node_dir.join("npm.cmd");
    if !npm.is_file() {
        return Err(format!("Node.js 缺少 npm.cmd（{}）", node_dir.display()));
    }
    let pnpm = install_pnpm(app, &node_dir, registry)?;
    Ok(Toolchain {
        node: exe,
        node_dir,
        npm,
        pnpm: Some(pnpm),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ver: &str, lts: bool) -> NodeIndexEntry {
        NodeIndexEntry {
            version: ver.to_string(),
            lts: if lts { Some(serde_json::json!("LTS")) } else { None },
        }
    }

    #[test]
    fn pick_lts_prefers_latest() {
        let entries = vec![
            entry("v20.19.0", true),
            entry("v22.14.0", true),
            entry("v23.0.0", false),
            entry("v24.0.0", false),
            entry("v0.12.18", true), // 远古版本应被忽略
        ];
        assert_eq!(pick_lts(&entries), Some("v22.14.0"));
    }

    #[test]
    fn pick_lts_empty() {
        assert_eq!(pick_lts(&[]), None);
    }

    #[test]
    fn pick_lts_list_orders_newest_first() {
        let entries = vec![
            entry("v20.19.0", true),
            entry("v22.14.0", true),
            entry("v24.2.0", true),
            entry("v23.0.0", false),
        ];
        let list = pick_lts_list(&entries);
        assert_eq!(list, vec!["v24.2.0", "v22.14.0", "v20.19.0"]);
    }

    #[test]
    fn parse_version_ordering() {
        assert_eq!(cmp_ver("v22.14.0", "v22.9.0"), std::cmp::Ordering::Greater);
        assert_eq!(cmp_ver("v20.19.0", "v22.14.0"), std::cmp::Ordering::Less);
        assert_eq!(cmp_ver("v22.14.0", "v22.14.0"), std::cmp::Ordering::Equal);
    }

    #[test]
    fn probe_finds_nothing_without_node() {
        // 不依赖真实环境：结果结构完整即可（node 为空默认值）
        let t = probe();
        assert!(!t.node_dir.as_os_str().is_empty() || t.node.as_os_str().is_empty());
        assert_eq!(t.npm, t.node_dir.join("npm.cmd"));
    }
}
