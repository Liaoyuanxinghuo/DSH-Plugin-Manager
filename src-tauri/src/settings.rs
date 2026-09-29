//! 应用设置：npm 镜像源、DSH 下载目录等（持久化到 data_dir/settings.json）

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 默认 registry（国内镜像，与用户 pnpm 配置一致）
pub const DEFAULT_REGISTRY: &str = "https://registry.npmmirror.com";
/// 默认 DSH 多版本下载根目录
pub const DEFAULT_DSH_DIR: &str = "C:\\dsh-versions";
/// 默认 GitHub 下载镜像（大陆无需 VPN 拉取 raw.githubusercontent / github 资源）
pub const DEFAULT_GITHUB_MIRROR: &str = "https://ghfast.top";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// npm registry 镜像源（npm search / 包信息 / 下载 DSH / 写入 profile .npmrc）
    pub npm_registry: String,
    /// DSH 多版本下载根目录
    pub dsh_download_dir: String,
    /// GitHub 下载镜像前缀（空 = 直连 GitHub）；用于整合包市场、raw 文件等
    pub github_mirror: String,
    /// 用户自行添加的额外 GitHub 镜像（轮流尝试，与 github_mirror 一起优先于内置镜像）
    pub github_mirrors: Vec<String>,
    /// 各 DSH 版本安装后的实际体积（学习式进度总量：首次安装记录，后续安装有准确百分比）
    pub dsh_install_sizes: std::collections::HashMap<String, u64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            npm_registry: DEFAULT_REGISTRY.to_string(),
            dsh_download_dir: DEFAULT_DSH_DIR.to_string(),
            github_mirror: DEFAULT_GITHUB_MIRROR.to_string(),
            github_mirrors: Vec::new(),
            dsh_install_sizes: std::collections::HashMap::new(),
        }
    }
}

fn settings_file() -> PathBuf {
    let dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.join("dsh-plugin-manager").join("settings.json")
}

pub fn load_settings() -> Settings {
    std::fs::read_to_string(settings_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_settings(s: &Settings) -> Result<(), String> {
    let path = settings_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let raw = serde_json::to_string_pretty(s).map_err(|e| format!("序列化失败: {e}"))?;
    std::fs::write(path, raw).map_err(|e| format!("写入设置失败: {e}"))
}

/// npm 官方 registry
pub const NPM_OFFICIAL: &str = "https://registry.npmjs.org";

/// 校验 registry 值；**空字符串表示不使用镜像源（走 npm 官方源）**
pub fn validate_registry(s: &str) -> Result<(), String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(());
    }
    if !s.starts_with("https://") && !s.starts_with("http://") {
        return Err("registry 需以 http:// 或 https:// 开头（留空 = 不使用镜像源）".to_string());
    }
    Ok(())
}

/// 规范化 registry（去除尾斜杠；空保持空）
pub fn normalize_registry(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() { String::new() } else { t.trim_end_matches('/').to_string() }
}

/// 解析最终 npm registry：空 = npm 官方源
pub fn resolve_registry(reg: &str) -> String {
    let t = reg.trim();
    if t.is_empty() { NPM_OFFICIAL.to_string() } else { normalize_registry(t) }
}

/// 解析 node 二进制镜像 base：空 = nodejs.org 官方 dist
pub fn resolve_node_base(reg: &str) -> String {
    let t = reg.trim();
    if t.is_empty() { "https://nodejs.org/dist".to_string() } else { format!("{}/-/binary/node", normalize_registry(t)) }
}

/// 校验 GitHub 镜像值：空 = 直连，或 http(s) 前缀
pub fn validate_github_mirror(s: &str) -> Result<(), String> {
    let t = s.trim();
    if t.is_empty() { return Ok(()); }
    if !t.starts_with("https://") && !t.starts_with("http://") {
        return Err("GitHub 镜像需以 http:// 或 https:// 开头（留空 = 直连 GitHub）".to_string());
    }
    Ok(())
}

/// 规范化镜像前缀（去尾斜杠、去空白）
pub fn normalize_github_mirror(s: &str) -> String {
    s.trim().trim_end_matches('/').to_string()
}

/// 校验并规范化用户自定义镜像列表（去空、去重、逐条校验 http(s)）
pub fn normalize_github_mirrors(list: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for (i, m) in list.iter().enumerate() {
        let t = normalize_github_mirror(m);
        if t.is_empty() {
            continue;
        }
        validate_github_mirror(&t).map_err(|e| format!("第 {} 个镜像无效：{e}", i + 1))?;
        if !out.iter().any(|x| x == &t) {
            out.push(t);
        }
    }
    Ok(out)
}

/// 用户配置的 GitHub 镜像链（主镜像 + 自定义列表，去重），供 ghnet 轮询
pub fn github_mirror_list() -> Vec<String> {
    let s = load_settings();
    let mut out: Vec<String> = Vec::new();
    let primary = normalize_github_mirror(&s.github_mirror);
    if !primary.is_empty() {
        out.push(primary);
    }
    for m in &s.github_mirrors {
        let t = normalize_github_mirror(m);
        if !t.is_empty() && !out.iter().any(|x| x == &t) {
            out.push(t);
        }
    }
    out
}

/// GitHub 域名是否应走镜像
fn is_github_url(url: &str) -> bool {
    url.starts_with("https://raw.githubusercontent.com/")
        || url.starts_with("https://github.com/")
        || url.starts_with("https://codeload.github.com/")
        || url.starts_with("https://objects.githubusercontent.com/")
        || url.starts_with("https://api.github.com/")
}

/// 给 GitHub 资源 URL 加镜像前缀；非 GitHub 域原样返回；镜像空 = 直连
pub fn github_proxy(url: &str, mirror: &str) -> String {
    let m = mirror.trim().trim_end_matches('/');
    if m.is_empty() || !is_github_url(url) {
        return url.to_string();
    }
    format!("{m}/{url}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_registry() {
        assert!(validate_registry("https://registry.npmmirror.com").is_ok());
        assert!(validate_registry("https://registry.npmjs.org/").is_ok());
        assert!(validate_registry("ftp://x").is_err());
        // 空 = 不使用镜像源，合法
        assert!(validate_registry("").is_ok());
        assert!(validate_registry("  ").is_ok());
        assert!(validate_registry("registry.npmmirror.com").is_err());
    }

    #[test]
    fn test_normalize_registry() {
        assert_eq!(normalize_registry("https://a.com/"), "https://a.com");
        assert_eq!(normalize_registry(" https://a.com "), "https://a.com");
    }

    #[test]
    fn test_github_proxy() {
        assert_eq!(github_proxy("https://raw.githubusercontent.com/a/b/main/index.json", "https://ghfast.top"),
            "https://ghfast.top/https://raw.githubusercontent.com/a/b/main/index.json");
        // 非 GitHub 域不改
        assert_eq!(github_proxy("https://registry.npmmirror.com/-/x", "https://ghfast.top"),
            "https://registry.npmmirror.com/-/x");
        // 镜像空 = 直连
        assert_eq!(github_proxy("https://raw.githubusercontent.com/a/b/c", ""),
            "https://raw.githubusercontent.com/a/b/c");
        assert!(validate_github_mirror("").is_ok());
        assert!(validate_github_mirror("https://ghfast.top").is_ok());
        assert!(validate_github_mirror("ftp://x").is_err());
    }

    #[test]
    fn test_settings_roundtrip() {
        let mut s = Settings::default();
        s.npm_registry = "https://registry.npmjs.org".to_string();
        save_settings(&s).unwrap();
        let loaded = load_settings();
        assert_eq!(loaded.npm_registry, "https://registry.npmjs.org");
        // 清理
        let _ = std::fs::remove_file(settings_file());
    }

    #[test]
    fn test_settings_serialize_camel_case() {
        // 前端按 camelCase 读取（Tauri 返回值不自动转换字段名）
        let s = Settings::default();
        let raw = serde_json::to_string(&s).unwrap();
        assert!(raw.contains("\"npmRegistry\""));
        assert!(raw.contains("\"dshDownloadDir\""));
        assert!(!raw.contains("npm_registry"));
    }
}
