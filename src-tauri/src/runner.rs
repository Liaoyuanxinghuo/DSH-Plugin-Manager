//! 进程调度：启动/停止 DSH、端口探测

use crate::models::*;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

/// 解析 dsh 的启动器：返回 (node.exe 全路径, bin.js 全路径)。
/// 绕过 dsh.cmd shim——shim 依赖 PATH 中的 node，应用进程 PATH 可能没有 node，
/// 而 DSH Desktop 等自带 node，直接用 node + bin.js 启动最可靠。
pub fn resolve_dsh_launcher(env: &DshEnv) -> Result<(String, String), String> {
    let bin = env.bin_path.as_ref().ok_or("该环境未绑定 dsh 可执行文件")?;
    let dir = Path::new(bin).parent().ok_or("无法定位 dsh 所在目录")?;

    // 1) bin.js：dsh 包入口（常见布局）
    let binjs_candidates: Vec<PathBuf> = [
        dir.join("node_modules").join("@deepseek-ai").join("dsh").join("lib").join("bin.js"),
        dir.join("node_modules").join("@deepseek-ai").join("dsh").join("bin.js"),
        dir.join("lib").join("bin.js"),
        dir.join("bin.js"),
    ]
    .into_iter()
    .collect();
    let binjs = binjs_candidates.iter().find(|p| p.is_file()).cloned().ok_or_else(|| {
        format!("未找到 dsh 包入口 bin.js（{}）", dir.display())
    })?;

    // 2) node.exe：优先 dsh 本体自带（DSH Desktop 布局 .dsh-win/node/node.exe），再常见位置
    let node_candidates: Vec<PathBuf> = vec![
        dir.join("node.exe"),
        dir.join("..").join("..").join("node").join("node.exe"), // versions/X → .dsh-win/node
        dir.join("..").join("node").join("node.exe"),
        dir.join("..").join("..").join("..").join("node").join("node.exe"),
    ];
    let node = node_candidates.iter().find(|p| p.is_file()).cloned().or_else(|| {
        // 回退：where node（PATH）
        let out = crate::scanner::run_cmd("where node");
        out.stdout
            .lines()
            .next()
            .map(|s| PathBuf::from(s.trim()))
            .filter(|p| p.is_file())
    }).or_else(|| {
        // 兜底：应用自装 / 探测到的 node（toolchain::probe 覆盖 runtime 目录、.dsh-win、npm 全局等）
        let t = crate::toolchain::probe();
        if t.node.is_file() { Some(t.node) } else { None }
    });
    let node = node.ok_or_else(|| "未找到 node.exe（PATH 中无 node，且 dsh 本体未自带）".to_string())?;
    Ok((node.to_string_lossy().to_string(), binjs.to_string_lossy().to_string()))
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
    if let Ok((node, binjs)) = resolve_dsh_launcher(env) {
        let mut cmd = Command::new(&node);
        cmd.arg(&binjs).arg("--profile").arg(profile).arg("--no-open");
        if port != 0 {
            cmd.arg("--port").arg(port.to_string());
        }
        if let Some(home) = dsh_home {
            cmd.env("DSH_HOME", home);
        }
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        cmd.stdout(Stdio::from(log_file.try_clone().map_err(|e| format!("日志失败: {e}"))?))
            .stderr(Stdio::from(err_file))
            .stdin(Stdio::null());
        let child = cmd.spawn().map_err(|e| format!("启动失败: {e}"))?;
        return Ok((child.id(), log_path.to_string_lossy().to_string()));
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