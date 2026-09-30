//! DSH 插件市场目录（awesome-dsh-plugin registry）
//! 数据源: https://awesome-dsh-plugin.com/plugins.json
//! 支持 curated 插件浏览（含 GitHub-only 插件，安装走 github:user/repo）

use serde::{Deserialize, Serialize};

const CATALOG_URL: &str = "https://awesome-dsh-plugin.com/plugins.json";

/// 市场目录
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketCatalog {
    pub name: String,
    pub count: usize,
    pub updated: String,
    /// 分类 key 列表（与插件 category 字段对应，如 memory/docs）
    pub categories: Vec<String>,
    /// 分类显示名（优先 zh-CN）
    pub category_labels: std::collections::HashMap<String, String>,
    pub plugins: Vec<MarketPlugin>,
}

/// 单个插件条目
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketPlugin {
    pub name: String,
    pub owner: String,
    pub url: String,
    pub category: String,
    pub description_zh: String,
    pub description_en: String,
    pub npm: Option<String>,
    pub version: Option<String>,
    pub stars: Option<i64>,
    pub downloads: Option<i64>,
    /// 安装 spec（npm 包名 或 github:owner/repo）
    pub install: String,
    /// 是否仅 GitHub 源（无 npm 包）
    pub github_only: bool,
}

/// 拉取并解析市场目录
pub fn market_catalog() -> Result<MarketCatalog, String> {
    let resp = reqwest::blocking::Client::new()
        .get(CATALOG_URL)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .map_err(|e| format!("拉取市场目录失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("市场目录服务返回状态 {}", resp.status()));
    }
    let raw: serde_json::Value = resp.json().map_err(|e| format!("解析市场目录失败: {e}"))?;

    // categories 在源站是 map：{ "memory": {"zh":"记忆","en":"Memory"}, ... }
    // 兼容数组写法；无 categories 时从插件里去重兜底
    let mut category_labels: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut categories: Vec<String> = Vec::new();
    match raw.get("categories") {
        Some(serde_json::Value::Object(m)) => {
            for (k, v) in m {
                let label = v
                    .get("zh")
                    .or_else(|| v.get("zh-CN"))
                    .or_else(|| v.get("en"))
                    .and_then(|x| x.as_str())
                    .unwrap_or(k)
                    .to_string();
                category_labels.insert(k.clone(), label);
                categories.push(k.clone());
            }
        }
        Some(serde_json::Value::Array(arr)) => {
            for v in arr {
                if let Some(s) = v.as_str() {
                    categories.push(s.to_string());
                }
            }
        }
        _ => {}
    }

    let mut plugins = Vec::new();
    if let Some(arr) = raw.get("plugins").and_then(|p| p.as_array()) {
        for p in arr {
            let desc_zh = p.get("description").and_then(|d| d.get("zh")).and_then(|v| v.as_str()).unwrap_or("");
            let desc_en = p.get("description").and_then(|d| d.get("en")).and_then(|v| v.as_str()).unwrap_or("");
            let install = p.get("install").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let npm = p.get("npm").and_then(|v| v.as_str()).map(String::from);
            // 解析安装 spec：从 install 命令提取（dsh plugin --profile X add <spec>）
            let spec = install
                .split_whitespace()
                .filter(|t| *t != "add" && !t.starts_with("dsh") && !t.starts_with("--profile") && *t != "web" && !t.starts_with("plugin"))
                .last()
                .unwrap_or("")
                .to_string();
            let spec = if spec.is_empty() {
                // 兜底：npm 名或 github 形式
                npm.clone().unwrap_or_else(|| format!("github:{}/{}", owner(p), name(p)))
            } else {
                spec
            };
            plugins.push(MarketPlugin {
                name: name(p).to_string(),
                owner: owner(p).to_string(),
                url: p.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                category: p.get("category").and_then(|v| v.as_str()).unwrap_or("other").to_string(),
                description_zh: desc_zh.to_string(),
                description_en: desc_en.to_string(),
                npm: npm.clone(),
                version: p.get("version").and_then(|v| v.as_str()).map(String::from),
                stars: p.get("stars").and_then(|v| v.as_i64()),
                downloads: p.get("downloads").and_then(|v| v.as_i64()),
                install: spec,
                github_only: npm.is_none(),
            });
        }
    }

    // 插件出现但未登记的分类 → 补进列表
    for p in &plugins {
        if !p.category.is_empty() && !categories.contains(&p.category) {
            categories.push(p.category.clone());
            category_labels
                .entry(p.category.clone())
                .or_insert_with(|| p.category.clone());
        }
    }
    categories.sort();

    Ok(MarketCatalog {
        name: raw.get("name").and_then(|v| v.as_str()).unwrap_or("dsh-market").to_string(),
        count: plugins.len(),
        updated: raw.get("updated").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        categories,
        category_labels,
        plugins,
    })
}

fn name(p: &serde_json::Value) -> &str {
    p.get("name").and_then(|v| v.as_str()).unwrap_or("")
}

fn owner(p: &serde_json::Value) -> &str {
    p.get("owner").and_then(|v| v.as_str()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_install_spec_github() {
        let install = "dsh plugin --profile web add github:CAI-MH/dsh-quality-review";
        let spec = install
            .split_whitespace()
            .filter(|t| *t != "add" && !t.starts_with("dsh") && !t.starts_with("--profile") && *t != "web" && !t.starts_with("plugin"))
            .last()
            .unwrap_or("")
            .to_string();
        assert_eq!(spec, "github:CAI-MH/dsh-quality-review");
    }

    #[test]
    fn test_parse_install_spec_npm() {
        let install = "dsh plugin --profile web add @anonyjcy/dsh-j-space";
        let spec = install
            .split_whitespace()
            .filter(|t| *t != "add" && !t.starts_with("dsh") && !t.starts_with("--profile") && *t != "web" && !t.starts_with("plugin"))
            .last()
            .unwrap_or("")
            .to_string();
        assert_eq!(spec, "@anonyjcy/dsh-j-space");
    }

    #[test]
    fn test_market_catalog_real() {
        // 真实拉取验证（网络可用时）
        match market_catalog() {
            Ok(cat) => {
                eprintln!(
                    "目录: {} 插件数 {} 分类数 {}",
                    cat.name,
                    cat.count,
                    cat.categories.len()
                );
                assert!(cat.count > 100, "目录应包含大量插件");
                let github_only = cat.plugins.iter().filter(|p| p.github_only).count();
                eprintln!("GitHub-only 插件: {github_only}");
                assert!(cat.plugins.iter().any(|p| p.github_only), "应存在 GitHub-only 插件");
            }
            Err(e) => {
                eprintln!("SKIP: 网络不可用 {e}");
            }
        }
    }
}
