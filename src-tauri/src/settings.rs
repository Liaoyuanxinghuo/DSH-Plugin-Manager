//! 应用设置：npm 镜像源、DSH 下载目录等（持久化到 data_dir/settings.json）

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 默认 registry（国内镜像，与用户 pnpm 配置一致）
pub const DEFAULT_REGISTRY: &str = "https://registry.npmmirror.com";
/// 默认 DSH 多版本下载根目录
pub const DEFAULT_DSH_DIR: &str = "C:\\dsh-versions";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// npm registry 镜像源（npm search / 包信息 / 下载 DSH / 写入 profile .npmrc）
    pub npm_registry: String,
    /// DSH 多版本下载根目录
    pub dsh_download_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            npm_registry: DEFAULT_REGISTRY.to_string(),
            dsh_download_dir: DEFAULT_DSH_DIR.to_string(),
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

/// 校验 registry 值
pub fn validate_registry(s: &str) -> Result<(), String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("registry 不能为空".to_string());
    }
    if !s.starts_with("https://") && !s.starts_with("http://") {
        return Err("registry 需以 http:// 或 https:// 开头".to_string());
    }
    if s.ends_with('/') {
        // 允许带尾斜杠，使用前统一去除
        Ok(())
    } else {
        Ok(())
    }
}

/// 规范化 registry（去除尾斜杠）
pub fn normalize_registry(s: &str) -> String {
    s.trim().trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_registry() {
        assert!(validate_registry("https://registry.npmmirror.com").is_ok());
        assert!(validate_registry("https://registry.npmjs.org/").is_ok());
        assert!(validate_registry("ftp://x").is_err());
        assert!(validate_registry("").is_err());
        assert!(validate_registry("registry.npmmirror.com").is_err());
    }

    #[test]
    fn test_normalize_registry() {
        assert_eq!(normalize_registry("https://a.com/"), "https://a.com");
        assert_eq!(normalize_registry(" https://a.com "), "https://a.com");
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
