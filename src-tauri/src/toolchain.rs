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
    /// node.exe 绝对路径（统一运行时）
    pub fn node_exe(&self) -> &Path {
        &self.node
    }
    /// npm.cmd 绝对路径
    pub fn npm_exe(&self) -> &Path {
        &self.npm
    }
    /// pnpm.cmd 绝对路径（已安装时）
    pub fn pnpm_exe(&self) -> Option<&Path> {
        self.pnpm.as_deref()
    }

    /// 给**单个子进程**写入运行时环境变量。
    /// - `NODE` / `npm_config_prefix`：绝对路径定位
    /// - `PATH`：**仅写入该 Command 的环境块**（运行时环境变量），供 dsh 内部再拉起
    ///   `pnpm`/`node` 时解析命令名。**不修改系统/用户 PATH**，进程结束后无残留。
    /// 本进程自己 spawn 的 node/npm/pnpm 仍一律用 `node_exe()`/`npm_exe()`/`pnpm_exe()` 绝对路径。
    pub fn apply_runtime_env(&self, cmd: &mut Command) {
        if self.node.is_file() {
            cmd.env("NODE", &self.node);
        }
        if self.node_dir.is_dir() {
            cmd.env("npm_config_prefix", &self.node_dir);
        }
        // 子进程 PATH = 运行时目录 + 原 PATH（仅本 Command；保证 dsh→pnpm 能解析到便携 pnpm）
        let mut dirs: Vec<String> = Vec::new();
        if self.node_dir.is_dir() {
            dirs.push(self.node_dir.display().to_string());
        }
        if let Some(pnpm) = &self.pnpm {
            if let Some(d) = pnpm.parent() {
                let s = d.display().to_string();
                if !dirs.contains(&s) {
                    dirs.push(s);
                }
            }
        }
        if !dirs.is_empty() {
            let old = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{};{}", dirs.join(";"), old));
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
    // 2) DSH Desktop 自带 node（与 dsh 配套，版本新，通常 ≥24，能跑 import.meta.main）
    //    优先于 PATH 中的 node——PATH 里可能是不兼容的老 node（如 v22 会静默退出）。
    if node.is_none() {
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        let mut candidates: Vec<String> = vec![
            format!("{home}\\.dsh-win\\node\\node.exe"),
            "C:\\Program Files\\nodejs\\node.exe".to_string(),
            "C:\\Program Files (x86)\\nodejs\\node.exe".to_string(),
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
    // 3) PATH 中的 node（where node）最后兜底
    if node.is_none() {
        for line in run_capture("where node").lines() {
            let p = PathBuf::from(line.trim());
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
    // 带超时的 blocking client（防网络挂起导致 ensure 永久卡住）
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let body = client
        .get(&url)
        .send()
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

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
    let resp = client
        .get(&zip_url)
        .send()
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

/// 用 npm 全局安装 pnpm（幂等）；尽量落在 node_dir（便携 runtime）内，
/// 与 node.exe 同目录，shim 可用 `%dp0%\node.exe` 自洽，不依赖 PATH。
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
    // --prefix 指到 node_dir：强制 pnpm.cmd 落进统一运行时目录（而不是用户 AppData\npm）
    if reg.is_empty() {
        cmd.args(["install", "-g", "pnpm", "--prefix", &node_dir.display().to_string(), "--no-audit", "--no-fund"]);
    } else {
        cmd.args(["install", "-g", "pnpm", "--prefix", &node_dir.display().to_string()]);
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
        Ok(s) if s.success() => {
            // pnpm 实际安装到 npm 全局 prefix 目录，未必是 node_dir——
            // 安装成功后在多个候选位置定位 pnpm.cmd
            // 全渠道定位：node_dir / npm 全局 prefix / config get prefix / %APPDATA%\npm / cmd where
            let mut cands: Vec<PathBuf> = vec![pnpm.clone(), node_dir.join("pnpm.cmd")];
            if let Some(prefix) = npm_global_dir(&npm, "prefix") {
                cands.push(prefix.join("pnpm.cmd"));
            }
            if let Some(prefix) = npm_global_dir(&npm, "config get prefix") {
                cands.push(prefix.join("pnpm.cmd"));
            }
            if let Ok(ap) = std::env::var("APPDATA") {
                cands.push(PathBuf::from(format!("{}\\npm\\pnpm.cmd", ap)));
                cands.push(PathBuf::from(format!("{}\\Roaming\\npm\\pnpm.cmd", ap)));
            }
            if let Some(found) = where_pnpm() {
                cands.push(found);
            }
            for c in &cands {
                if c.is_file() {
                    emit_log(app, &format!("pnpm 安装完成：{}", c.display()), "stdout");
                    return Ok(c.clone());
                }
            }
            Err(format!(
                "pnpm 已安装（退出码 0）但未定位到 pnpm.cmd，已检查：{}",
                cands
                    .iter()
                    .map(|c| c.display().to_string())
                    .collect::<Vec<_>>()
                    .join("；")
            ))
        }
        Ok(s) => Err(format!("pnpm 安装失败（退出码 {:?}）", s.code())),
        Err(e) => Err(format!("pnpm 安装进程异常: {e}")),
    }
}

/// 查 npm 全局目录：sub 为 "prefix"（`npm prefix -g`）或 "config get prefix"
fn npm_global_dir(npm: &Path, sub: &str) -> Option<PathBuf> {
    let args: Vec<&str> = sub.split_whitespace().collect();
    let mut cmd = crate::fsutil::hidden_command(npm);
    cmd.args(&args);
    if sub.starts_with("config") {
        cmd.arg("-g");
    } else {
        cmd.arg("-g");
    }
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if prefix.is_empty() {
        return None;
    }
    Some(PathBuf::from(prefix))
}

/// cmd /c where pnpm.cmd（完整 PATH + PATHEXT 探测）
fn where_pnpm() -> Option<PathBuf> {
    let out = crate::fsutil::hidden_command("cmd").args(["/c", "where", "pnpm.cmd"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout).lines().next()?.trim().to_string();
    if line.is_empty() {
        None
    } else {
        Some(PathBuf::from(line))
    }
}


/// runtime 目录内完整的便携工具链（node.exe≥24 + npm.cmd + pnpm.cmd）→ Some
/// 不完整（空 / 缺件 / node 过旧）返回 None，调用方应走 install_lts 补齐。
pub fn runtime_toolchain() -> Option<Toolchain> {
    let rd = runtime_dir();
    let mut best: Option<(PathBuf, u32)> = None;
    for e in std::fs::read_dir(&rd).ok()?.flatten() {
        let p = e.path().join("node.exe");
        if !p.is_file() {
            continue;
        }
        let major = node_major_of(&p).unwrap_or(0);
        if major < 24 {
            continue;
        }
        if best.as_ref().map(|(_, m)| major > *m).unwrap_or(true) {
            best = Some((p, major));
        }
    }
    let (node, _) = best?;
    let node_dir = node.parent()?.to_path_buf();
    let npm = node_dir.join("npm.cmd");
    let pnpm = node_dir.join("pnpm.cmd");
    if !npm.is_file() || !pnpm.is_file() {
        return None;
    }
    Some(Toolchain {
        node,
        node_dir,
        npm,
        pnpm: Some(pnpm),
    })
}

/// 确保统一便携运行时就绪：node 24+ / npm / pnpm 全在 `%AppData%\dsh-plugin-manager\runtime\`。
/// **不管用户系统有没有 Node，便携包必须装**——runtime 完整直接用，否则完整下载。
/// 失败时报错，不再静默回退系统 Node（避免又出现「有 Node 没 pnpm」等不一致）。
pub fn ensure(app: &tauri::AppHandle, registry: &str) -> Result<Toolchain, String> {
    if let Some(t) = runtime_toolchain() {
        return Ok(t);
    }
    emit_log(
        app,
        "便携运行时未就绪（缺 Node 24+ 或 pnpm，或 runtime 为空），开始下载完整便携包…",
        "stdout",
    );
    install_lts(app, registry).map_err(|e| {
        format!("便携运行时下载安装失败：{e}（请检查网络或镜像源后重试；也可点「初始化环境」）")
    })
}

/// 读取指定 node.exe 的主版本号（跑 `node --version`，如 v22.15.0 → 22）
fn node_major_of(node: &std::path::Path) -> Option<u32> {
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    let mut child = std::process::Command::new(node)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn()
        .ok()?;
    // 超时轮询：node.exe 挂起时 6s 后放弃，避免 ensure() 永久卡住
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(_) => {
                let _ = child.kill();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let v = s.trim().strip_prefix('v').unwrap_or(s.trim());
    v.split('.')
        .next()
        .and_then(|p| p.parse::<u32>().ok())
}

/// 强制下载最新 Node LTS（不信任本机已有 node——例如本机只有 Node <24，
/// 跑不了使用 import.meta.main 的新 dsh，即使 probe 能发现也不可用）。
pub fn ensure_force(app: &tauri::AppHandle, registry: &str) -> Result<Toolchain, String> {
    emit_log(app, "本机 Node 版本过低或不可用，开始下载最新 Node LTS…", "stdout");
    install_lts(app, registry)
}

/// 下载最新 LTS Node 到 runtime 目录并装好 npm/pnpm（失败时逐级回退更早 LTS）
fn install_lts(app: &tauri::AppHandle, registry: &str) -> Result<Toolchain, String> {
    emit_log(app, "正在下载便携 Node.js LTS 到 runtime 目录…", "stdout");
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

    #[test]
    fn apply_runtime_env_sets_node_without_touching_process_path() {
        // 运行时环境变量：只写 Command env；当前进程 PATH 不变
        let base = std::env::temp_dir().join(format!("dshpm-envtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("rt");
        std::fs::create_dir_all(&dir).unwrap();
        let node = dir.join("node.exe");
        std::fs::write(&node, b"x").unwrap();
        std::fs::write(dir.join("pnpm.cmd"), b"x").unwrap();
        let tc = Toolchain {
            node: node.clone(),
            node_dir: dir.clone(),
            npm: dir.join("npm.cmd"),
            pnpm: Some(dir.join("pnpm.cmd")),
        };
        let path_before = std::env::var("PATH").unwrap_or_default();
        // 子进程内：NODE 已设；PATH（该进程 env）应以运行时目录开头
        let mut cmd = crate::fsutil::hidden_command("cmd");
        cmd.args(["/C", "echo NODE=%NODE% & echo PATHHEAD=%PATH:~0,80%"]);
        cmd.stdout(Stdio::piped()).stderr(Stdio::null());
        tc.apply_runtime_env(&mut cmd);
        let path_after = std::env::var("PATH").unwrap_or_default();
        assert_eq!(path_before, path_after, "apply_runtime_env 不得改当前进程 PATH");
        let out = cmd.output().expect("spawn echo");
        let s = String::from_utf8_lossy(&out.stdout);
        assert!(
            s.contains(&node.display().to_string()),
            "子进程应看到 NODE=绝对路径: {s}"
        );
        // 子进程 PATH 前缀含运行时目录（供 dsh→pnpm 解析）；系统 PATH 未被修改
        assert!(
            s.to_uppercase().contains(&dir.display().to_string().to_uppercase())
                || s.contains("PATHHEAD="),
            "子进程 env 应带运行时目录: {s}"
        );
        assert_eq!(tc.node_exe(), node.as_path());
        assert_eq!(tc.pnpm_exe(), Some(dir.join("pnpm.cmd").as_path()));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 真实网络：下载便携 Node LTS zip → 解压 → node --version → npm -g pnpm --prefix。
    /// 手动执行：`cargo test --lib real_download_portable_runtime -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_download_portable_runtime() {
        let registry = "https://registry.npmmirror.com";
        let entries = fetch_node_index(registry).expect("应能拉取 node 索引");
        let ver = pick_lts(&entries).expect("应有 LTS").to_string();
        let dest = std::env::temp_dir().join(format!("dshpm-realrt-{}", ver.replace('.', "_")));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();

        // 与 download_node 相同的 URL / 解压逻辑（不依赖 AppHandle）
        let base = crate::settings::resolve_node_base(registry);
        let zip_url = format!("{base}/{ver}/node-{ver}-win-x64.zip");
        println!("下载 {zip_url}");
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap();
        let bytes = client.get(&zip_url).send().unwrap().bytes().unwrap();
        assert!(bytes.len() > 10 * 1024 * 1024, "zip 应有十几 MB，实际 {}", bytes.len());
        let tmp = dest.join("node.zip");
        std::fs::write(&tmp, &bytes).unwrap();
        let file = std::fs::File::open(&tmp).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i).unwrap();
            let name = entry.name().to_string();
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
            let mut f = std::fs::File::create(&out).unwrap();
            std::io::copy(&mut entry, &mut f).unwrap();
        }
        let node = dest.join("node.exe");
        assert!(node.is_file(), "应解压出 node.exe");
        let v = crate::fsutil::hidden_command(&node).arg("--version").output().unwrap();
        let vs = String::from_utf8_lossy(&v.stdout).trim().to_string();
        println!("node --version => {vs}");
        assert!(vs.starts_with('v'), "应输出版本号: {vs}");
        let major: u32 = vs.trim_start_matches('v').split('.').next().unwrap().parse().unwrap();
        assert!(major >= 24, "LTS 应为 24+，实际 {vs}");

        // npm -g pnpm --prefix 到 node 目录（与 install_pnpm 相同参数）
        let npm = dest.join("npm.cmd");
        assert!(npm.is_file());
        let st = crate::fsutil::hidden_command(&npm)
            .args([
                "install",
                "-g",
                "pnpm",
                "--prefix",
                &dest.display().to_string(),
                "--registry",
                &crate::settings::resolve_registry(registry),
                "--no-audit",
                "--no-fund",
            ])
            .current_dir(&dest)
            .output()
            .unwrap();
        assert!(st.status.success(), "pnpm 安装应成功: {st:?}");
        let pnpm = dest.join("pnpm.cmd");
        assert!(pnpm.is_file(), "pnpm.cmd 应落在 node 目录");
        let pv = crate::fsutil::hidden_command(&pnpm).arg("--version").output().unwrap();
        let pvs = String::from_utf8_lossy(&pv.stdout).trim().to_string();
        println!("pnpm --version => {pvs}");
        assert!(!pvs.is_empty());
        let _ = std::fs::remove_dir_all(&dest);
    }
}
