//! 文件系统快捷操作：资源管理器打开、URL 打开

use std::process::Command;

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
    match Command::new("explorer.exe").arg(path).spawn() {
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
    match Command::new("rundll32.exe")
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
}
