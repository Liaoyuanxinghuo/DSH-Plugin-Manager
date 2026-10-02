//! 文件系统快捷操作：资源管理器打开、URL 打开、隐藏控制台子进程

use std::process::Command;

/// 构造**后台**子进程命令：Windows 下带 CREATE_NO_WINDOW，不弹 cmd/PowerShell 黑框。
/// GUI 应用（explorer / rundll32 / 浏览器）不受影响，照常显示自己的窗口；
/// 控制台程序（powershell / cmd / node / npm / tasklist…）则静默执行。
/// 所有非交互 spawn 都应走这里，别直接 Command::new。
pub fn hidden_command<S: AsRef<std::ffi::OsStr>>(program: S) -> Command {
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// 在 Windows 资源管理器中打开指定路径。
/// explorer.exe 是单实例进程（通常已有桌面外壳在运行），spawn 失败时明确报错，
/// 避免"点了没反应"的静默失败。
pub fn open_in_explorer(path: &str) -> Result<(), String> {
    let path = path.trim();
    if path.is_empty() {
        return Err("路径为空".to_string());
    }
    if !std::path::Path::new(path).exists() {
        return Err(format!("路径不存在: {path}"));
    }
    match hidden_command("explorer.exe").arg(path).spawn() {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("无法打开资源管理器: {e}")),
    }
}

/// 使用默认浏览器打开 URL
/// 注意：不能用 `explorer.exe <url>`——带查询参数/token 的 URL（如 dsh web 的
/// http://127.0.0.1:3080/?token=...）会被 explorer 误判为文件路径而打开资源管理器。
/// 改用 rundll32 url.dll,FileProtocolHandler（Windows 官方协议分发入口），
/// 交给默认浏览器处理，且不会弹出 cmd 黑框。
pub fn open_url(url: &str) -> Result<(), String> {
    if url.is_empty() {
        return Err("URL 为空".to_string());
    }
    match hidden_command("rundll32.exe")
        .args(["url.dll", "FileProtocolHandler", url])
        .spawn()
    {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("无法打开浏览器: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_empty() {
        assert!(open_in_explorer("").is_err());
        assert!(open_url("").is_err());
    }

    #[test]
    fn test_open_valid_url_no_panic() {
        // 打开 URL 不应 panic（是否真正打开浏览器不做断言，避免测试副作用）
        let _ = open_url("https://example.com");
    }

    /// 功能测试：hidden_command 的子进程必须没有控制台窗口（GetConsoleWindow()==0）。
    /// GUI 应用（Tauri）启动后台 powershell/cmd 时不闪黑框，靠的就是这个标志。
    #[test]
    fn hidden_command_child_has_no_console_window() {
        let script = r#"Add-Type -Name W -Namespace N -MemberDefinition '[DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow();'; [N.W]::GetConsoleWindow().ToInt64()"#;
        let out = hidden_command("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output()
            .expect("spawn powershell");
        assert!(out.status.success(), "stderr={}", String::from_utf8_lossy(&out.stderr));
        let s = String::from_utf8_lossy(&out.stdout);
        assert_eq!(s.trim(), "0", "hidden_command 子进程不应持有控制台窗口, got={s}");
    }

    /// 功能测试：hidden_command 管道输出仍可用（隐藏≠吞输出）
    #[test]
    fn hidden_command_captures_stdout() {
        let out = hidden_command("cmd")
            .args(["/C", "echo hidden-ok"])
            .output()
            .expect("spawn cmd");
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("hidden-ok"));
    }
}
