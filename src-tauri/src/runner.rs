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
/// 必须打开该 URL 才能完成认证（裸地址会显示 authentication required）。
pub fn find_auth_url(log_path: &str) -> Option<String> {
    let content = std::fs::read_to_string(log_path).ok()?;
    for line in content.lines() {
        let line = line.trim();
        if let Some(pos) = line.find("http://127.0.0.1:") {
            let url = line[pos..].trim_end_matches(|c: char| c.is_whitespace() || c == '。' || c == '，');
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }
    }
    None
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
    fn find_auth_url_none_when_no_url() {
        let tmp = std::env::temp_dir().join(format!("dshpm-auth2-{}", std::process::id()));
        std::fs::write(&tmp, "starting...\nno url here\n").unwrap();
        assert!(find_auth_url(tmp.to_string_lossy().as_ref()).is_none());
        std::fs::remove_file(&tmp).unwrap();
    }
}