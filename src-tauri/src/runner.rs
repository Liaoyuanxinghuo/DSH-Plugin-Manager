//! 进程调度：启动/停止 DSH、端口探测

use crate::models::*;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::time::{SystemTime, UNIX_EPOCH};

/// 解析 dsh 的启动器：返回 (node.exe 全路径, bin.js 全路径)。
/// 绕过 dsh.cmd shim——shim 依赖 PATH 中的 node，应用进程 PATH 可能没有 node，
/// 而 DSH Desktop 等自带 node，直接用 node + bin.js 启动最可靠。
/// 启动 node 探测结果缓存（同会话内免重复探测；key = run_command）
static NODE_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
fn node_cache() -> &'static Mutex<HashMap<String, String>> {
    NODE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn resolve_dsh_launcher(env: &DshEnv) -> Result<(String, String), String> {
    // bin_path 可能为空（部分手动添加/下载安装的环境）；回退从 run_command 的第一个路径段解析。
    // 这样不依赖 PATH 也能用 node + bin.js 直接启动，避免 cmd shim 找不到 node 秒退。
    let bin = env
        .bin_path
        .as_ref()
        .map(|b| b.clone())
        .or_else(|| env.run_command.split_whitespace().next().map(|s| s.to_string()))
        .ok_or("该环境未绑定 dsh 可执行文件")?;
    let dir = Path::new(&bin).parent().ok_or("无法定位 dsh 所在目录")?;

    // 1) bin.js：dsh 包入口（常见布局）
    // 注意 bin 可能位于 node_modules\.bin\dsh.cmd（npm 安装布局）：
    // .bin 的上一级就是 node_modules，bin.js 实际在 ..\@deepseek-ai\dsh\lib\bin.js。
    let binjs_candidates: Vec<PathBuf> = [
        dir.join("node_modules").join("@deepseek-ai").join("dsh").join("lib").join("bin.js"),
        dir.join("node_modules").join("@deepseek-ai").join("dsh").join("bin.js"),
        dir.join("..").join("@deepseek-ai").join("dsh").join("lib").join("bin.js"),
        dir.join("..").join("@deepseek-ai").join("dsh").join("bin.js"),
        dir.join("..").join("..").join("node_modules").join("@deepseek-ai").join("dsh").join("lib").join("bin.js"),
        dir.join("..").join("..").join("node_modules").join("@deepseek-ai").join("dsh").join("bin.js"),
        dir.join("lib").join("bin.js"),
        dir.join("bin.js"),
    ]
    .into_iter()
    .collect();
    let binjs = binjs_candidates.iter().find(|p| p.is_file()).cloned().ok_or_else(|| {
        format!("未找到 dsh 包入口 bin.js（{}）", dir.display())
    })?;

    // 2) node.exe：收集本机全部候选，逐个探活（`node bin.js --version` 有非空输出才可用——
    //    老 node 如 v22 跑新 dsh 的 `import.meta.main` 会静默退出），
    //    优先选 ≥24 的最高版本；没有 24+ 则选最高可用版本并告警。
    let mut candidates: Vec<PathBuf> = vec![
        dir.join("node.exe"),
        dir.join("..").join("..").join("node").join("node.exe"), // versions/X → .dsh-win/node
        dir.join("..").join("node").join("node.exe"),
        dir.join("..").join("..").join("..").join("node").join("node.exe"),
    ];
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    candidates.extend([
        PathBuf::from("C:\\Program Files\\nodejs\\node.exe"),
        PathBuf::from("C:\\Program Files (x86)\\nodejs\\node.exe"),
        PathBuf::from(format!("{home}\\.dsh-win\\node\\node.exe")),
        PathBuf::from(format!("{appdata}\\npm\\node.exe")),
    ]);
    // 应用自装 runtime 目录
    if let Ok(rd) = std::fs::read_dir(crate::toolchain::runtime_dir()) {
        for e in rd.flatten() {
            let p = e.path().join("node.exe");
            if p.is_file() {
                candidates.push(p);
            }
        }
    }
    // PATH 中的 node（where node）全部
    let out = crate::scanner::run_cmd("where node");
    for line in out.stdout.lines() {
        let p = PathBuf::from(line.trim());
        if p.is_file() {
            candidates.push(p);
        }
    }
    // 去重（保留首个出现）
    let mut seen: Vec<PathBuf> = Vec::new();
    candidates.retain(|p| {
        if seen.iter().any(|s| s == p) {
            false
        } else {
            seen.push(p.clone());
            true
        }
    });
    // 0) 命中缓存：node 仍存在且能跑该 dsh → 秒回（不再逐候选探测）
    let cache_key = env.run_command.trim().to_string();
    if let Some(np) = node_cache().lock().unwrap().get(&cache_key).cloned() {
        let np_path = Path::new(&np);
        if np_path.is_file() && node_can_run(np_path, &binjs) {
            return Ok((np, binjs.to_string_lossy().to_string()));
        }
    }

    // 1) 并行探测候选的 node 版本（只跑 `node --version`，3s 超时；卡死的候选不影响整体）
    let handles: Vec<_> = candidates
        .iter()
        .filter(|c| c.is_file())
        .map(|c| {
            let c = c.clone();
            std::thread::spawn(move || node_version(&c).map(|v| (c.clone(), v)))
        })
        .collect();
    let mut usable: Vec<(PathBuf, (u32, u32))> = Vec::new();
    for h in handles {
        if let Ok(Some(x)) = h.join() {
            usable.push(x);
        }
    }

    // 2) 按「≥24 优先 + 版本降序」排序，从最优开始逐个做 bin.js 探活（命中即返回）
    //    （Node ≥24 才能跑使用 import.meta.main 的新 dsh；只对最终选中做真实启动验证）
    let mut pool = usable.clone();
    pool.sort_by(|a, b| {
        let pa = (a.1 .0 >= 24) as u8;
        let pb = (b.1 .0 >= 24) as u8;
        pb.cmp(&pa).then_with(|| b.1.cmp(&a.1))
    });
    let mut tried: Vec<PathBuf> = Vec::new();
    for (node, _) in &pool {
        if tried.iter().any(|t| t == node) {
            continue;
        }
        tried.push(node.clone());
        if node_can_run(node, &binjs) {
            node_cache()
                .lock()
                .unwrap()
                .insert(cache_key.clone(), node.to_string_lossy().to_string());
            return Ok((node.to_string_lossy().to_string(), binjs.to_string_lossy().to_string()));
        }
    }

    Err("未找到可运行该 DSH 的 Node（将尝试自动安装 Node LTS 24；请检查网络或镜像源）".to_string())
}

/// 从可用 node 列表中选：优先 ≥24 的最高版本；没有 24+ 则选最高可用版本（并告警）。
fn choose_best_node(usable: &[(PathBuf, (u32, u32))]) -> Option<PathBuf> {
    let m24 = usable
        .iter()
        .filter(|(_, v)| v.0 >= 24)
        .max_by_key(|(_, v)| *v);
    if let Some(x) = m24 {
        return Some(x.0.clone());
    }
    let mx = usable.iter().max_by_key(|(_, v)| *v);
    if let Some(x) = mx {
        eprintln!(
            "DSH Manager: 未找到 Node >= 24，使用最高可用版本 {:?}（新 dsh 可能无法运行）",
            x.0
        );
        return Some(x.0.clone());
    }
    None
}

/// 解析 `node --version` 输出（如 "v24.21.0" / "24.21.0"）→ (主版本, 次版本)
fn parse_node_version(s: &str) -> Option<(u32, u32)> {
    let s = s.trim().strip_prefix('v').unwrap_or(s.trim());
    let mut it = s.split('.');
    let major: u32 = it.next()?.trim().parse().ok()?;
    let minor: u32 = it.next()?.trim().parse().ok()?;
    Some((major, minor))
}

/// 实测指定 node 的版本（超时保护）
fn node_version(node: &std::path::Path) -> Option<(u32, u32)> {
    use std::process::Stdio;
    let mut child = std::process::Command::new(node)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
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
    parse_node_version(&String::from_utf8_lossy(&out.stdout))
}

/// 实测指定 node 能否运行该 dsh：执行 `node bin.js --version`，
/// 有非空 stdout 视为可用（老 node 会静默退出、无输出）。带超时防止卡死。
fn node_can_run(node: &std::path::Path, binjs: &std::path::Path) -> bool {
    use std::process::Stdio;
    let mut child = match std::process::Command::new(node)
        .arg(binjs)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000) // CREATE_NO_WINDOW，不弹黑框
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    // 最多等 6 秒；超时视为不可用并杀掉
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(_) => {
                let _ = child.kill();
                return false;
            }
        }
    }
    let out = child.wait_with_output();
    match out {
        Ok(o) => !o.stdout.is_empty(),
        Err(_) => false,
    }
}

/// 构造启动命令：优先 node + bin.js（PATH 无关），回退原 shim 命令
pub fn build_start_command(env: &DshEnv, profile: &str, port: u16) -> String {
    if let Ok((node, binjs)) = resolve_dsh_launcher(env) {
        let mut cmd = format!("\"{node}\" \"{binjs}\" --profile {profile} --no-open");
        if port != 0 {
            cmd.push_str(&format!(" --port {port}"));
        }
        return cmd;
    }
    // 回退：<run_command> --profile <profile> --no-open [--port <port>]
    let mut cmd = env.run_command.clone();
    cmd.push_str(" --profile ");
    cmd.push_str(profile);
    cmd.push_str(" --no-open");
    if port != 0 {
        cmd.push_str(&format!(" --port {port}"));
    }
    cmd
}

/// 从 dsh web 日志中提取带认证 token 的访问 URL。
/// dsh web 启动后打印 `dsh web: http://127.0.0.1:<port>/?token=...`，
/// 必须打开该 URL 才能完成认证（裸地址会显示 authentication required / 401）。
/// **优先返回带 `token=` 的 URL**：日志里可能先出现其他 127.0.0.1 地址（裸地址、
/// 插件打印的本机 URL 等），若取第一条会永远拿不到 token、卡在「认证地址生成中」。
pub fn find_auth_url(log_path: &str) -> Option<String> {
    let content = std::fs::read_to_string(log_path).ok()?;
    let mut first_any: Option<String> = None;
    for line in content.lines() {
        let line = line.trim();
        let pos = line
            .find("http://127.0.0.1:")
            .or_else(|| line.find("http://localhost:"));
        if let Some(pos) = pos {
            let url = line[pos..].trim_end_matches(|c: char| c.is_whitespace() || c == '。' || c == '，');
            if url.is_empty() {
                continue;
            }
            if url.contains("token=") {
                return Some(url.to_string());
            }
            if first_any.is_none() {
                first_any = Some(url.to_string());
            }
        }
    }
    first_any
}

/// 等待日志中出现带 token 的 web URL（dsh 的 `dsh web:` 行常在 MCP 初始化完成后才打印）。
/// 每隔 `interval_ms` 重读一次日志，直到找到或超过 `timeout_ms`。
pub fn find_auth_url_wait(log_path: &str, timeout_ms: u64, interval_ms: u64) -> Option<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        if let Some(u) = find_auth_url(log_path) {
            return Some(u);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(interval_ms.max(50)));
    }
}

/// 解析「打开界面」应打开的 URL：**只认带 token 的认证地址**。
/// 裸地址在浏览器没有当前进程的 dsh cookie 时返回 401
/// （`dsh web authentication required; reopen the URL printed by dsh web.`），
/// 且每个进程 token 不同、重启后旧 cookie 失效，因此绝不回退裸地址。
/// 等不到 token 行则报错，让用户稍后再试。
pub fn resolve_web_open_url(log_path: &str, timeout_ms: u64) -> Result<String, String> {
    if log_path.trim().is_empty() {
        return Err("缺少运行日志，无法获取认证地址".to_string());
    }
    match find_auth_url_wait(log_path, timeout_ms, 500) {
        Some(url) if url.contains("token=") => Ok(url),
        Some(_) => Err("日志中的地址缺少认证 token，请查看运行日志".to_string()),
        None => Err("认证地址尚未出现（MCP 初始化可能仍在进行），请稍后再试".to_string()),
    }
}

/// DSH CLI 语义：profile 位于 `$DSH_HOME/profiles/<name>`。
/// 本应用管理的 profiles_dir 是直接包含 profile 子目录的目录，
/// 因此注入的 DSH_HOME 应指向它的父目录。
pub fn dsh_home_of(profiles_dir: &std::path::Path) -> String {
    profiles_dir
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 启动 DSH（后台、脱离当前控制台），stdout/stderr 重定向到临时日志文件
/// 返回 (进程 pid, 日志文件路径)；进程若启动失败秒退，可通过日志诊断
/// dsh_home：该 profile 所在的 profiles 目录（注入 DSH_HOME，实现任意 dsh × 任意来源 profile 组合）
pub fn start_dsh(env: &DshEnv, profile: &str, port: u16, dsh_home: Option<&str>) -> Result<(u32, String), String> {
    let log_path = std::env::temp_dir().join(format!(
        "dshpm-run-{}-{}.log",
        profile,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    ));
    let log_file = std::fs::File::create(&log_path).map_err(|e| format!("创建日志失败: {e}"))?;
    let err_file = log_file
        .try_clone()
        .map_err(|e| format!("创建日志失败: {e}"))?;

    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;

    // 优先：node + bin.js 直接启动（绕开 dsh.cmd shim，不依赖 PATH）
    let launcher = resolve_dsh_launcher(env);
    if let Ok((node, binjs)) = &launcher {
        let mut cmd = Command::new(node);
        cmd.arg(binjs).arg("--profile").arg(profile).arg("--no-open");
        if port != 0 {
            cmd.arg("--port").arg(port.to_string());
        }
        if let Some(home) = dsh_home {
            cmd.env("DSH_HOME", home);
        }
        // 关键：把工具链目录注入 dsh 进程的 PATH。
        // dsh 自身用绝对路径启动没问题，但它内部（如 web 界面装插件）会 spawn
        // node / npm / pnpm——这些依赖进程 PATH。若不注入，从零自动安装的 node
        // 在 dsh 内部就"找不到"，插件安装会失败。
        let mut path_extra = String::new();
        if let Some(nd) = std::path::Path::new(node).parent() {
            path_extra.push_str(&nd.to_string_lossy());
            path_extra.push(';');
        }
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        let global_npm = std::path::PathBuf::from(&appdata).join("npm");
        if global_npm.join("pnpm.cmd").is_file() {
            path_extra.push_str(&global_npm.to_string_lossy());
            path_extra.push(';');
        }
        let full_path = format!("{path_extra}{}", std::env::var("PATH").unwrap_or_default());
        cmd.env("PATH", full_path);
        // 国内镜像：uvx/pip 走 pypi 镜像、npm 走用户设置 registry（否则 MCP 装包 pypi.org 超时卡 1-2 分钟）
        apply_mirror_envs(&mut cmd);
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        cmd.stdout(Stdio::from(log_file.try_clone().map_err(|e| format!("日志失败: {e}"))?))
            .stderr(Stdio::from(err_file))
            .stdin(Stdio::null());
        let child = cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
        return Ok((child.id(), log_path.to_string_lossy().to_string()));
    }

    // resolve 失败：区分两类——
    // ① node 不可用（本机 node <24 或缺失）：绝不降级 shim（shim 同样会因 node
    //    不可用而静默失败，且把错误吞成"启动后立即退出"，上层无法触发自动装 node）。
    //    直接返回 Err，由 start_dsh_cmd 自动下载 Node LTS 24 后重试。
    // ② 布局问题（bin.js 找不到等）：保留 shim 回退（如 DSH Desktop 的 dsh.cmd
    //    自带 node，能自洽运行）。
    if let Err(e) = &launcher {
        if launcher_err_blocks_shim(e) {
            return Err(e.clone());
        }
    }

    // 回退：cmd /C 包装 shim 命令
    let cmd_str = build_start_command(env, profile, port);
    let full = format!("chcp 65001 >nul && {}", cmd_str);
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", &full]);
    if let Some(home) = dsh_home {
        cmd.env("DSH_HOME", home);
    }
    // 国内镜像：uvx/pip 走 pypi 镜像、npm 走用户设置 registry
    apply_mirror_envs(&mut cmd);
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    cmd.stdout(Stdio::from(log_file))
        .stderr(Stdio::from(err_file))
        .stdin(Stdio::null());
    let child = cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
    Ok((child.id(), log_path.to_string_lossy().to_string()))
}

/// 该 launcher 错误是否应阻断 shim 降级：
/// node 不可用（缺失或 <24）时降级 shim 必然同样失败（shim 也用 PATH 里的 node），
/// 且会把错误吞成"启动后立即退出"，使上层无法触发自动安装——必须直接报错。
/// 布局类错误（bin.js 找不到等）则可降级（如 dsh.cmd 自带 node 能自洽运行）。
fn launcher_err_blocks_shim(e: &str) -> bool {
    e.contains("可运行该 DSH 的 Node")
}

/// 进程是否存活（tasklist 探测；直接执行避免 cmd 引号嵌套问题）
pub fn is_pid_alive(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW：禁止弹出控制台黑框
        .output();
    match out {
        Ok(o) => {
            o.status.success() && String::from_utf8_lossy(&o.stdout).contains(&format!("{pid}"))
        }
        Err(_) => false,
    }
}

/// 批量探测 PID 存活集合（单次 tasklist 列全表，避免每 PID 起一个子进程）。
/// 必须在阻塞线程池调用——内部会 spawn 子进程，绝不能跑在主线程。
pub fn alive_pids(pids: &[u32]) -> std::collections::HashSet<u32> {
    use std::os::windows::process::CommandExt;
    use std::collections::HashSet;
    let mut alive = HashSet::new();
    if pids.is_empty() {
        return alive;
    }
    let out = Command::new("tasklist")
        .args(["/FO", "CSV", "/NH"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output();
    match out {
        Ok(o) if o.status.success() => {
            // 行格式："name.exe","1234","Console","1,234 K"
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                let line = line.trim();
                if !line.starts_with('"') {
                    continue;
                }
                let fields: Vec<&str> = line.split("\",\"").collect();
                if fields.len() < 2 {
                    continue;
                }
                if let Ok(pid) = fields[1].parse::<u32>() {
                    if pids.contains(&pid) {
                        alive.insert(pid);
                    }
                }
            }
            alive
        }
        _ => {
            // tasklist 失败：退回逐个探测
            for &p in pids {
                if is_pid_alive(p) {
                    alive.insert(p);
                }
            }
            alive
        }
    }
}

/// 读取日志文件尾部（诊断 dsh 启动失败原因）
pub fn read_log_tail(log_path: &str, lines: usize) -> String {
    match std::fs::read_to_string(log_path) {
        Ok(content) => {
            let all: Vec<&str> = content.lines().collect();
            let start = all.len().saturating_sub(lines);
            all[start..].join("\n")
        }
        Err(_) => String::new(),
    }
}

/// 停止 DSH 进程树（Windows taskkill /T /F）。
/// 宽容判定：taskkill 对树中已退出的子进程会报 "could not be terminated"（噪音，退出码非 0）；
/// 只要主 PID 不再存活，就视为已停止。
pub fn stop_dsh(pid: u32) -> Result<(), String> {
    let out = crate::scanner::run_cmd(&format!("taskkill /PID {pid} /T /F"));
    if out.success || !is_pid_alive(pid) {
        Ok(())
    } else {
        // 主进程仍存活：补一次强杀（不带 /T，避免子进程噪音干扰判定）
        let out2 = crate::scanner::run_cmd(&format!("taskkill /PID {pid} /F"));
        if out2.success || !is_pid_alive(pid) {
            Ok(())
        } else {
            Err(format!("停止失败: {}", out2.stderr.trim()))
        }
    }
}

/// 探测端口是否已被占用
pub fn is_port_open(port: u16) -> bool {
    TcpStream::connect(("127.0.0.1", port)).is_ok()
}

/// 探测 web 是否已在 HTTP 服务（任意响应即算，含 401 认证页）。
/// TCP 通 ≠ web 可访问；这里发一次极简 GET，看到 `HTTP/` 响应行即认为已起。
/// 浏览器若持有 dsh 签名 cookie（此前访问过 token 地址），裸地址可直接用。
pub fn probe_web_serving(port: u16) -> bool {
    use std::io::{Read, Write};
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(800))
    else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(800)));
    let _ = stream.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    let mut buf = [0u8; 16];
    match stream.read(&mut buf) {
        Ok(n) => n >= 5 && &buf[..5] == b"HTTP/",
        Err(_) => false,
    }
}

/// 国内 pypi 镜像（uvx/pip 走镜像，避免 pypi.org 大陆超时导致 MCP 启动卡 1-2 分钟）
pub const PYPI_MIRROR: &str = "https://pypi.tuna.tsinghua.edu.cn/simple";

/// DSH 子进程的国内镜像环境变量（uv / uvx / pip / npm）。
/// 纯函数便于测试；`npm_registry` 传用户设置的 registry（默认 npmmirror）。
pub fn mirror_env_pairs(npm_registry: &str) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = vec![
        ("UV_INDEX_URL".into(), PYPI_MIRROR.into()),
        ("UV_DEFAULT_INDEX".into(), PYPI_MIRROR.into()),
        ("PIP_INDEX_URL".into(), PYPI_MIRROR.into()),
    ];
    if !npm_registry.trim().is_empty() {
        v.push(("npm_config_registry".into(), npm_registry.trim().to_string()));
    }
    v
}

/// 给 DSH 子进程注入国内镜像环境（只写该 Command 的环境块，不改系统/用户环境）
fn apply_mirror_envs(cmd: &mut Command) {
    let registry = crate::settings::load_settings().npm_registry;
    for (k, v) in mirror_env_pairs(&registry) {
        cmd.env(k, v);
    }
}

/// 按空白切分命令行并去引号（支持带空格的引号路径）
fn cmdline_tokens(cmdline: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in cmdline.chars() {
        match c {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 命令行中 `--profile` 参数是否精确等于给定 profile（防 `web` 误配 `web2`）
pub fn profile_arg_matches(cmdline: &str, profile: &str) -> bool {
    let toks = cmdline_tokens(cmdline);
    toks.iter().enumerate().any(|(i, t)| {
        if t == "--profile" {
            toks.get(i + 1).map(|p| p == profile).unwrap_or(false)
        } else {
            t.strip_prefix("--profile=").map(|p| p == profile).unwrap_or(false)
        }
    })
}

/// 从命令行提取 `--port N`（无则 0）
pub fn parse_cmdline_port(cmdline: &str) -> u16 {
    let toks = cmdline_tokens(cmdline);
    toks.iter()
        .enumerate()
        .find_map(|(i, t)| {
            if t == "--port" {
                toks.get(i + 1)?.parse::<u16>().ok()
            } else {
                t.strip_prefix("--port=").and_then(|v| v.parse::<u16>().ok())
            }
        })
        .unwrap_or(0)
}

/// 严格孤儿判定：命令行包含目标 bin.js 路径（大小写/反斜杠归一）**且** `--profile` 精确匹配。
/// 已知 PID（running 表）由调用方排除。
pub fn orphan_cmdline_match(cmdline: &str, binjs: &str, profile: &str) -> bool {
    if binjs.trim().is_empty() || !profile_arg_matches(cmdline, profile) {
        return false;
    }
    let norm = |s: &str| s.to_lowercase().replace("\\\\", "\\");
    norm(cmdline).contains(&norm(binjs))
}

/// 列出机器上的 node.exe 进程（PID + 命令行）。PowerShell WMI 查询，必须在阻塞线程池调用。
fn list_node_processes() -> Vec<(u32, String)> {
    use std::os::windows::process::CommandExt;
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Get-CimInstance Win32_Process -Filter \"Name='node.exe'\" | Select-Object ProcessId,CommandLine | ConvertTo-Json -Compress",
        ])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output();
    let text = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Vec::new(),
    };
    let val: serde_json::Value = match serde_json::from_str(text.trim()) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let items: Vec<&serde_json::Value> = match &val {
        serde_json::Value::Array(a) => a.iter().collect(),
        serde_json::Value::Object(_) => vec![&val],
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|it| {
            let pid = it.get("ProcessId")?.as_u64()? as u32;
            let cmd = it
                .get("CommandLine")
                .and_then(|c| c.as_str())
                .unwrap_or_default()
                .to_string();
            Some((pid, cmd))
        })
        .collect()
}

/// 查找目标 env×profile 的孤儿 node 进程（同 bin.js + 同 profile 名，且不在 known_pids 中）。
/// 只查不杀——清理必须由用户在 UI 上确认。
pub fn find_orphans(env: &DshEnv, profile: &str, known_pids: &[u32]) -> Vec<crate::models::OrphanProcess> {
    let binjs = resolve_dsh_launcher(env)
        .map(|(_, b)| b)
        .unwrap_or_default();
    list_node_processes()
        .into_iter()
        .filter_map(|(pid, cmd)| {
            if known_pids.contains(&pid) || !orphan_cmdline_match(&cmd, &binjs, profile) {
                return None;
            }
            Some(crate::models::OrphanProcess {
                pid,
                port: parse_cmdline_port(&cmd),
                cmdline: cmd,
            })
        })
        .collect()
}

/// 从 start 起找第一个空闲端口（用于多实例自动分配，避免端口冲突）
pub fn find_free_port(start: u16) -> Option<u16> {
    (start..=65535).find(|&p| !is_port_open(p))
}

/// 生成带时间戳的 RunningProcess
pub fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let jdn = days as i64 + 2440588;
    let a = jdn + 32044;
    let b = (4 * a + 3) / 146097;
    let c = a - 146097 * b / 4;
    let d = (4 * c + 3) / 1461;
    let e = c - 1461 * d / 4;
    let mo = (5 * e + 2) / 153;
    let day = e - (153 * mo + 2) / 5 + 1;
    let month = mo + 3 - 12 * (mo / 10);
    let year = 100 * b + d - 4800 + mo / 10;
    format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn real_start_with_empty_bin_path() {
        // 动态探测本机真实 dsh 布局（标准 npm 布局），没有可用布局则跳过——
        // ignored 测试对机器状态敏感，硬编码路径可能因版本被清理而失效。
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let mut found: Option<PathBuf> = None;
        for root in [
            PathBuf::from("C:\\dsh-versions"),
            PathBuf::from(format!("{home}\\.dsh-win\\versions")),
        ] {
            if let Ok(rd) = std::fs::read_dir(&root) {
                for e in rd.flatten() {
                    let b = e
                        .path()
                        .join("node_modules")
                        .join("@deepseek-ai")
                        .join("dsh")
                        .join("lib")
                        .join("bin.js");
                    if b.is_file() {
                        found = Some(e.path());
                        break;
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }
        let Some(dir) = found else {
            println!("本机无可用 dsh 布局，跳过真实启动验证");
            return;
        };
        let run_cmd = if dir.join("node_modules\\.bin\\dsh.cmd").is_file() {
            dir.join("node_modules\\.bin\\dsh.cmd")
        } else if dir.join("dsh.cmd").is_file() {
            dir.join("dsh.cmd")
        } else {
            dir.join("node_modules\\@deepseek-ai\\dsh\\lib\\bin.js")
        };
        let env = DshEnv {
            id: "real".into(),
            name: dir.file_name().unwrap().to_string_lossy().to_string(),
            source: crate::models::EnvSource::Manual,
            version: "0.1.7-rc.2".into(),
            home_dir: "C:\\Users\\xiaolei\\.dsh".into(),
            run_command: run_cmd.to_string_lossy().to_string(),
            bin_path: None,
            scan_profiles_dir: None,
        };
        let (node, binjs) = resolve_dsh_launcher(&env).expect("应解析出 launcher");
        println!("node={node}\nbinjs={binjs}");
        assert!(std::path::Path::new(&binjs).is_file());
        let (pid, log_path) =
            start_dsh(&env, "base", 39161, Some("C:\\Users\\xiaolei\\.dsh"))
                .expect("start_dsh 应成功");
        std::thread::sleep(std::time::Duration::from_secs(8));
        let content = std::fs::read_to_string(&log_path).unwrap_or_default();
        println!("== 日志（{} 字节）==\n{content}", content.len());
        assert!(
            content.contains("dsh web:"),
            "日志应有 dsh web 启动输出，实际: {content}"
        );
        // 进程存活检查
        let alive = is_pid_alive(pid);
        println!("进程存活: {alive}");
        assert!(alive);
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
    }

    #[test]
    fn launcher_err_blocks_shim_logic() {
        // node 不可用 → 必须阻断 shim（否则吞错误、无法自动装）
        assert!(launcher_err_blocks_shim("未找到可运行该 DSH 的 Node（将尝试自动安装 Node LTS 24…）"));
        // 布局类错误 → 允许 shim 降级
        assert!(!launcher_err_blocks_shim("未找到 dsh 包入口 bin.js（C:\\x）"));
        assert!(!launcher_err_blocks_shim("该环境未绑定 dsh 可执行文件"));
    }

    #[test]
    fn node_version_reads_real_node() {
        // where node 找到的 node 应能读出版本（主版本 ≥ 1）
        let out = crate::scanner::run_cmd("where node");
        let p = out
            .stdout
            .lines()
            .next()
            .map(|s| PathBuf::from(s.trim()))
            .filter(|p| p.is_file());
        if let Some(p) = p {
            assert!(node_version(&p).is_some());
        }
        // 本机 .dsh-win\node 若存在应为 v24
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let p24 = PathBuf::from(format!("{home}\\.dsh-win\\node\\node.exe"));
        if p24.is_file() {
            let v = node_version(&p24);
            assert!(v.is_some(), ".dsh-win node 应能读出版本");
            assert_eq!(v.unwrap().0, 24, ".dsh-win 自带 node 应为 v24");
        }
    }

    #[test]
    fn parse_node_version_parses_v_output() {
        assert_eq!(parse_node_version("v24.21.0"), Some((24, 21)));
        assert_eq!(parse_node_version("24.21.0"), Some((24, 21)));
        assert_eq!(parse_node_version("v22.15.0"), Some((22, 15)));
        assert_eq!(parse_node_version("v20.11.1"), Some((20, 11)));
        assert_eq!(parse_node_version("  v24.0.0  "), Some((24, 0)));
        assert_eq!(parse_node_version("abc"), None);
        assert_eq!(parse_node_version(""), None);
    }

    #[test]
    fn choose_best_node_prefers_24_plus_highest() {
        let v22 = PathBuf::from("C:\\node22");
        let v24 = PathBuf::from("C:\\node24");
        let v26 = PathBuf::from("C:\\node26");
        // 有 24+ 时选其中最高的
        let list = vec![(v22.clone(), (22, 15)), (v24.clone(), (24, 21)), (v26.clone(), (26, 1))];
        assert_eq!(choose_best_node(&list), Some(v26));
        // 并列版本取先出现的（v24 在 v22 前）
        let list2 = vec![(v24.clone(), (24, 21)), (v22.clone(), (22, 15))];
        assert_eq!(choose_best_node(&list2), Some(v24));
        // 没有 24+ 时取最高（并告警）
        let list3 = vec![(v22.clone(), (22, 15))];
        assert_eq!(choose_best_node(&list3), Some(v22));
        // 空列表
        assert_eq!(choose_best_node(&[]), None);
    }

    #[test]
    fn node_can_run_detects_working_js() {
        // 临时 js 正常输出 → node_can_run 应 true（本机任意 node 都应能跑 console.log）
        let tmp = std::env::temp_dir().join(format!(
            "dshpm-js-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        std::fs::write(&tmp, "console.log('ok');").unwrap();
        assert!(node_can_run(Path::new("node"), &tmp));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn resolve_launcher_without_bin_path() {
        // bin_path 为空（旧版手动添加/下载安装的 env），只有 run_command 指向 .bin\dsh.cmd
        let tmp = std::env::temp_dir().join(format!(
            "dshpm-ln2-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let nm = tmp.join("node_modules");
        let bin_dir = nm.join(".bin");
        let pkg = nm.join("@deepseek-ai").join("dsh");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(pkg.join("lib")).unwrap();
        std::fs::write(bin_dir.join("dsh.cmd"), "@echo off").unwrap();
        std::fs::write(pkg.join("lib").join("bin.js"), "console.log('0.1.7-rc.1');").unwrap();
        let shim = bin_dir.join("dsh.cmd").to_string_lossy().to_string();
        let env = DshEnv {
            id: "t".into(),
            name: "t".into(),
            source: crate::models::EnvSource::Manual,
            version: "0.1.7-rc.1".into(),
            home_dir: "C:\\Users\\t\\.dsh".into(),
            run_command: shim,
            bin_path: None,
            scan_profiles_dir: None,
        };
        let (node, binjs) = resolve_dsh_launcher(&env).expect("run_command 应能解析 launcher");
        assert!(std::path::Path::new(&node).is_file());
        assert!(std::path::Path::new(&binjs).is_file());
        assert!(binjs.ends_with("lib\\bin.js"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_launcher_from_bin_layout() {
        // 模拟 npm 安装布局：bin_path = node_modules\.bin\dsh.cmd
        let tmp = std::env::temp_dir().join(format!(
            "dshpm-ln-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let nm = tmp.join("node_modules");
        let bin_dir = nm.join(".bin");
        let pkg = nm.join("@deepseek-ai").join("dsh");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(pkg.join("lib")).unwrap();
        std::fs::write(bin_dir.join("dsh.cmd"), "@echo off").unwrap();
        std::fs::write(pkg.join("lib").join("bin.js"), "console.log('0.1.7-rc.1');").unwrap();
        let shim = bin_dir.join("dsh.cmd").to_string_lossy().to_string();
        let env = DshEnv {
            id: "t".into(),
            name: "t".into(),
            source: crate::models::EnvSource::Manual,
            version: "0.1.7-rc.1".into(),
            home_dir: "C:\\Users\\t\\.dsh".into(),
            run_command: shim.clone(),
            bin_path: Some(shim),
            scan_profiles_dir: None,
        };
        let (node, binjs) = resolve_dsh_launcher(&env).expect("应解析出 launcher");
        assert!(std::path::Path::new(&binjs).is_file());
        assert!(binjs.ends_with("lib\\bin.js"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
    use super::*;
    use crate::models::EnvSource;

    fn test_env() -> DshEnv {
        DshEnv {
            id: "t".into(),
            name: "test".into(),
            source: EnvSource::GlobalCli,
            version: "0.1.0-rc.7".into(),
            home_dir: ".".into(),
            run_command: "dsh".into(),
            bin_path: None,
            scan_profiles_dir: None,
        }
    }

    #[test]
    fn test_build_start_command() {
        let cmd = build_start_command(&test_env(), "web", 3080);
        assert!(cmd.contains("--profile web"));
        assert!(cmd.contains("--port 3080"));
    }

    #[test]
    fn test_build_start_command_no_port() {
        let cmd = build_start_command(&test_env(), "headless", 0);
        assert!(cmd.contains("--profile headless"));
        assert!(!cmd.contains("--port"));
    }

    #[test]
    fn test_find_free_port_returns_open_port() {
        // 返回的端口不应被占用（此时必然空闲）
        let p = find_free_port(3080).unwrap();
        assert!(!is_port_open(p));
        assert!(p >= 3080);
    }

    #[test]
    fn test_find_free_port_skips_occupied() {
        // 占用一个临时端口后，分配应从其后继续找空闲端口
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let occupied = listener.local_addr().unwrap().port();
        let p = find_free_port(occupied).unwrap();
        assert_ne!(p, occupied, "不应返回已被占用的端口");
        assert!(!is_port_open(p));
    }

    #[test]
    fn test_stop_dsh_dead_pid_is_ok() {
        // 不存在的 PID：taskkill 会报错，但进程已不在 → 宽容判定视为已停止
        let r = stop_dsh(999999);
        assert!(r.is_ok(), "已退出的进程停止应视为成功，实际: {r:?}");
    }

    #[test]
    fn test_is_pid_alive_current_and_dead() {
        // 当前进程必然存活；一个不存在的 PID 必然已死
        assert!(is_pid_alive(std::process::id()));
        assert!(!is_pid_alive(1));
    }

    #[test]
    fn test_resolve_launcher_real_dshwin() {
        // 真实 DSH Desktop 布局：应解析出 .dsh-win/node/node.exe 与包内 bin.js
        let d = Path::new("C:/Users/xiaolei/.dsh-win/versions/0.1.7-rc.2-mc");
        if !d.join("dsh.cmd").is_file() {
            eprintln!("SKIP: DSH Desktop 目录不存在");
            return;
        }
        let env = DshEnv {
            id: "t".into(),
            name: "test".into(),
            source: EnvSource::Manual,
            version: "0.1.7-rc.2".into(),
            home_dir: ".".into(),
            run_command: d.join("dsh.cmd").to_string_lossy().to_string(),
            bin_path: Some(d.join("dsh.cmd").to_string_lossy().to_string()),
            scan_profiles_dir: None,
        };
        let (node, binjs) = resolve_dsh_launcher(&env).expect("应解析出启动器");
        assert!(node.to_lowercase().contains("node.exe"), "node 应为 node.exe: {node}");
        assert!(binjs.ends_with("bin.js"), "binjs 应为 bin.js: {binjs}");
        // 构造出的启动命令应包含 node 与 bin.js
        let cmd = build_start_command(&env, "0.1.7-rc.2-mc", 3080);
        assert!(cmd.contains("node.exe"));
        assert!(cmd.contains("bin.js"));
        assert!(cmd.contains("--profile 0.1.7-rc.2-mc"));
    }

    #[test]
    fn test_resolve_launcher_fallback_unknown() {
        // 无法解析的环境应回退到原始 run_command
        let env = DshEnv {
            id: "t".into(),
            name: "test".into(),
            source: EnvSource::GlobalCli,
            version: "0.1.0-rc.7".into(),
            home_dir: ".".into(),
            run_command: "dsh".into(),
            bin_path: None,
            scan_profiles_dir: None,
        };
        let cmd = build_start_command(&env, "web", 3080);
        assert!(cmd.starts_with("dsh --profile web"));
    }

    #[test]
    fn test_read_log_tail_returns_last_lines() {
        let path = std::env::temp_dir().join("dshpm-logtail-test.log");
        std::fs::write(&path, "line1\nline2\nline3\nline4\nline5\n").unwrap();
        let tail = read_log_tail(path.to_string_lossy().as_ref(), 2);
        assert_eq!(tail, "line4\nline5");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_read_log_tail_missing_file_is_empty() {
        assert!(read_log_tail("Z:\\no\\such\\file.log", 5).is_empty());
    }

    #[test]
    fn find_auth_url_extracts_token_url() {
        let tmp = std::env::temp_dir().join(format!("dshpm-auth-{}", std::process::id()));
        std::fs::write(&tmp, "dsh web: http://127.0.0.1:3080/?token=abc123\ndsh web: opening the default browser\n").unwrap();
        let url = find_auth_url(tmp.to_string_lossy().as_ref()).unwrap();
        assert_eq!(url, "http://127.0.0.1:3080/?token=abc123");
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn find_auth_url_prefers_token_over_earlier_bare_url() {
        // 日志里可能先出现裸地址/其他本机 URL，必须跳到带 token 的那条
        let tmp = std::env::temp_dir().join(format!("dshpm-auth-pref-{}", std::process::id()));
        std::fs::write(
            &tmp,
            "listening on http://127.0.0.1:3080/\nplugin probe http://127.0.0.1:9229\n\ndsh web: http://127.0.0.1:3080/?token=xyz789\n",
        )
        .unwrap();
        let url = find_auth_url(tmp.to_string_lossy().as_ref()).unwrap();
        assert_eq!(url, "http://127.0.0.1:3080/?token=xyz789");
        std::fs::remove_file(&tmp).unwrap();
    }

    /// 功能测试：start_dsh 用 File::create 持有写句柄传给子进程，
    /// find_auth_url 必须仍能从同一路径读到 token（Windows 文件共享）。
    #[test]
    fn find_auth_url_reads_while_writer_handle_held() {
        use std::io::Write;
        let tmp = std::env::temp_dir().join(format!("dshpm-locked-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        // 与 start_dsh 相同的创建方式：句柄保持打开
        let mut writer = std::fs::File::create(&tmp).unwrap();
        writer
            .write_all(b"booting...\ndsh web: http://127.0.0.1:3081/?token=held-open\n")
            .unwrap();
        writer.flush().unwrap();
        // writer 不 drop = 子进程继承句柄期间文件仍被占用
        let url = find_auth_url(tmp.to_string_lossy().as_ref())
            .expect("写句柄占用期间必须仍可读到 token");
        assert_eq!(url, "http://127.0.0.1:3081/?token=held-open");
        // resolve_web_open_url 同样要能在占用期间拿到
        let url2 = resolve_web_open_url(tmp.to_string_lossy().as_ref(), 500).unwrap();
        assert_eq!(url2, "http://127.0.0.1:3081/?token=held-open");
        drop(writer);
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn find_auth_url_none_when_no_url() {
        let tmp = std::env::temp_dir().join(format!("dshpm-auth2-{}", std::process::id()));
        std::fs::write(&tmp, "starting...\nno url here\n").unwrap();
        assert!(find_auth_url(tmp.to_string_lossy().as_ref()).is_none());
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn find_auth_url_matches_localhost_variant() {
        let tmp = std::env::temp_dir().join(format!("dshpm-auth3-{}", std::process::id()));
        std::fs::write(&tmp, "dsh web: http://localhost:3080/?token=xyz\n").unwrap();
        let url = find_auth_url(tmp.to_string_lossy().as_ref()).unwrap();
        assert_eq!(url, "http://localhost:3080/?token=xyz");
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn find_auth_url_wait_picks_up_url_written_later() {
        // 日志先为空，稍后写入 token URL → 等待函数应能等到
        let tmp = std::env::temp_dir().join(format!("dshpm-auth-wait-{}", std::process::id()));
        std::fs::write(&tmp, "booting...\n").unwrap();
        let writer = {
            let p = tmp.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(300));
                std::fs::write(&p, "booting...\ndsh web: http://127.0.0.1:3080/?token=late\n").unwrap();
            })
        };
        let url = find_auth_url_wait(tmp.to_string_lossy().as_ref(), 3000, 100)
            .expect("应等到稍后写入的 token URL");
        assert_eq!(url, "http://127.0.0.1:3080/?token=late");
        writer.join().unwrap();
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn find_auth_url_wait_times_out_when_never_appears() {
        let tmp = std::env::temp_dir().join(format!("dshpm-auth-to-{}", std::process::id()));
        std::fs::write(&tmp, "no web line ever\n").unwrap();
        assert!(
            find_auth_url_wait(tmp.to_string_lossy().as_ref(), 200, 50).is_none(),
            "日志始终无 URL 应返回 None"
        );
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn resolve_web_open_url_returns_token_url() {
        let tmp = std::env::temp_dir().join(format!("dshpm-open-{}", std::process::id()));
        std::fs::write(&tmp, "dsh web: http://127.0.0.1:3080/?token=abc123\n").unwrap();
        let url = resolve_web_open_url(tmp.to_string_lossy().as_ref(), 1000).unwrap();
        assert_eq!(url, "http://127.0.0.1:3080/?token=abc123");
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn resolve_web_open_url_rejects_bare_address() {
        // 裸地址无 token → 401，不允许作为打开目标
        let tmp = std::env::temp_dir().join(format!("dshpm-open-bare-{}", std::process::id()));
        std::fs::write(&tmp, "dsh web: http://127.0.0.1:3080/\n").unwrap();
        let err = resolve_web_open_url(tmp.to_string_lossy().as_ref(), 800)
            .unwrap_err();
        assert!(err.contains("token"), "裸地址应报缺少 token：{err}");
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn resolve_web_open_url_times_out_without_token_line() {
        let tmp = std::env::temp_dir().join(format!("dshpm-open-to-{}", std::process::id()));
        std::fs::write(&tmp, "starting mcp...\n").unwrap();
        let err = resolve_web_open_url(tmp.to_string_lossy().as_ref(), 300).unwrap_err();
        assert!(err.contains("尚未出现"), "无 token 行应报尚未出现：{err}");
        std::fs::remove_file(&tmp).unwrap();
    }

    #[test]
    fn resolve_web_open_url_empty_log_is_error() {
        assert!(resolve_web_open_url("", 300).is_err(), "空日志路径应报错");
    }

    #[test]
    fn mirror_env_pairs_has_pypi_and_npm_registry() {
        let pairs = mirror_env_pairs("https://registry.npmmirror.com");
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        // uvx/pip 必须走 pypi 镜像（大陆 pypi.org 超时卡 MCP 启动 1-2 分钟）
        assert_eq!(get("UV_INDEX_URL"), PYPI_MIRROR);
        assert_eq!(get("UV_DEFAULT_INDEX"), PYPI_MIRROR);
        assert_eq!(get("PIP_INDEX_URL"), PYPI_MIRROR);
        // npm 走用户设置的 registry
        assert_eq!(get("npm_config_registry"), "https://registry.npmmirror.com");
        // 空 registry 不注入该项（保留用户本机 npm 配置）
        let pairs2 = mirror_env_pairs("  ");
        assert!(pairs2.iter().all(|(k, _)| k != "npm_config_registry"));
    }

    #[test]
    fn profile_arg_matches_exact_only() {
        // 命令行里 --profile 必须精确等于目标名（web 不匹配 web2）
        let c = r#""node.exe" "C:\a\bin.js" --profile web --no-open --port 3080"#;
        assert!(profile_arg_matches(c, "web"));
        assert!(!profile_arg_matches(c, "web2"));
        assert!(!profile_arg_matches(c, "we"));
        // --profile=name 形式
        let c2 = r#""node.exe" bin.js --profile=harness --no-open"#;
        assert!(profile_arg_matches(c2, "harness"));
        assert!(!profile_arg_matches(c2, "harness-1"));
        // 无 --profile 参数
        assert!(!profile_arg_matches(r#"node.exe bin.js"#, "web"));
    }

    #[test]
    fn parse_cmdline_port_variants() {
        assert_eq!(
            parse_cmdline_port(r#""node.exe" bin.js --profile web --no-open --port 3080"#),
            3080
        );
        assert_eq!(parse_cmdline_port("node.exe bin.js --port=10722"), 10722);
        assert_eq!(parse_cmdline_port("node.exe bin.js --profile web"), 0);
    }

    #[test]
    fn orphan_cmdline_match_strict() {
        let binjs = r"C:\Users\t\.dsh-win\versions\0.1.7\node_modules\@deepseek-ai\dsh\lib\bin.js";
        let cmd = format!(r#""node" "{}" --profile web --no-open --port 3080"#, binjs);
        // 同 bin.js + 同 profile → 孤儿候选
        assert!(orphan_cmdline_match(&cmd, binjs, "web"));
        // 同名前缀的其他 profile → 不算
        assert!(!orphan_cmdline_match(&cmd, binjs, "web2"));
        // 其他 bin.js（如另一个 DSH 版本）→ 不算（严格按 bin.js 归属）
        let other_binjs = r"C:\Users\t\.dsh-win\versions\0.1.6\node_modules\@deepseek-ai\dsh\lib\bin.js";
        assert!(!orphan_cmdline_match(&cmd, other_binjs, "web"));
        // 命令行不带 --profile → 不算
        assert!(!orphan_cmdline_match("node.exe something.js", binjs, "web"));
        // 空 binjs → 不算（无法归属，宁可漏不误杀）
        assert!(!orphan_cmdline_match(&cmd, "", "web"));
        // 反斜杠/大小写归一：同一路径不同写法也算
        let cmd2 = format!(r#""NODE" "{}" --profile web"#, binjs.to_uppercase());
        assert!(orphan_cmdline_match(&cmd2, binjs, "web"));
    }
}