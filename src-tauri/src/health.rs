//! Profile 依赖健康检查 + 残留缓存扫描

use serde::{Deserialize, Serialize};
use std::path::Path;

/// 依赖问题条目
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DepIssue {
    pub name: String,
    pub declared: String,
    pub installed: bool,
    pub issue: String, // "ok" | "missing"
}

/// 残留/缓存目录条目
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JunkEntry {
    pub name: String,
    pub path: String,
    pub size: u64,
}

/// 检查 profile 依赖完整性：package.json 声明 vs node_modules 实际存在
pub fn check_profile_deps(profile_dir: &Path) -> Result<Vec<DepIssue>, String> {
    let pkg_raw = std::fs::read_to_string(profile_dir.join("package.json"))
        .map_err(|e| format!("读取 package.json 失败: {e}"))?;
    let pkg: serde_json::Value = serde_json::from_str(&pkg_raw)
        .map_err(|e| format!("package.json 解析失败: {e}"))?;
    let deps = pkg
        .get("dependencies")
        .and_then(|d| d.as_object())
        .ok_or_else(|| "package.json 缺少 dependencies".to_string())?;

    let nm = profile_dir.join("node_modules");
    let mut issues = Vec::new();
    for (name, spec) in deps {
        let installed = dep_installed(&nm, name);
        issues.push(DepIssue {
            name: name.clone(),
            declared: spec.as_str().unwrap_or("").to_string(),
            installed,
            issue: if installed { "ok".to_string() } else { "missing".to_string() },
        });
    }
    issues.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(issues)
}

/// 判断依赖是否已安装（pnpm 布局：顶层或 @scope 下）
fn dep_installed(nm: &Path, name: &str) -> bool {
    let mut parts = name.splitn(2, '/');
    if name.starts_with('@') {
        let scope = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        return nm.join(scope).join(rest).is_dir() || nm.join(scope).join(rest).exists();
    }
    nm.join(name).exists()
}

/// 扫描 profile 内可清理的缓存/残留目录
/// 覆盖：.generations（dsh bundle 版本残留）、.dsh-market、.plugin-manager、
/// node_modules/.cache、.turbo 等。
pub fn scan_junk(profile_dir: &Path) -> Vec<JunkEntry> {
    let mut out = Vec::new();
    let candidates = [
        ".generations",
        ".dsh-market",
        ".plugin-manager",
        ".turbo",
        ".cache",
    ];
    for c in candidates {
        let p = profile_dir.join(c);
        if p.is_dir() {
            out.push(JunkEntry {
                name: c.to_string(),
                path: p.to_string_lossy().to_string(),
                size: dir_size(&p),
            });
        }
    }
    // node_modules/.cache
    let nm_cache = profile_dir.join("node_modules").join(".cache");
    if nm_cache.is_dir() {
        out.push(JunkEntry {
            name: "node_modules/.cache".to_string(),
            path: nm_cache.to_string_lossy().to_string(),
            size: dir_size(&nm_cache),
        });
    }
    out.sort_by(|a, b| b.size.cmp(&a.size));
    out
}

/// 删除指定残留项（按 JunkEntry 的 name 匹配）
pub fn clean_junk(profile_dir: &Path, names: &[String]) -> Result<Vec<String>, String> {
    let mut removed = Vec::new();
    for n in names {
        // 防路径穿越：只接受白名单名
        if !["generations", "dsh-market", "plugin-manager", "turbo", "cache", "node_modules/.cache"]
            .contains(&n.as_str())
        {
            return Err(format!("不允许清理的目标: {n}"));
        }
        let p = match n.as_str() {
            "node_modules/.cache" => profile_dir.join("node_modules").join(".cache"),
            _ => profile_dir.join(format!(".{n}")),
        };
        if p.is_dir() {
            std::fs::remove_dir_all(&p).map_err(|e| format!("清理 {n} 失败: {e}"))?;
            removed.push(n.clone());
        }
    }
    Ok(removed)
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(md) = std::fs::metadata(&p) {
                total += md.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_deps_ok_and_missing() {
        let work = std::env::temp_dir().join(format!("dshpm-hlth-{}", std::process::id()));
        if work.exists() {
            let _ = std::fs::remove_dir_all(&work);
        }
        let profile = work.join("p");
        std::fs::create_dir_all(profile.join("node_modules").join("@deepseek-ai")).unwrap();
        std::fs::write(
            profile.join("package.json"),
            r#"{"dependencies":{"@deepseek-ai/dsh-mcp-client":"0.0.1","dshmarket":"^1.0"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(profile.join("node_modules").join("@deepseek-ai").join("dsh-mcp-client")).unwrap();

        let issues = check_profile_deps(&profile).unwrap();
        assert_eq!(issues.len(), 2);
        let mcp = issues.iter().find(|i| i.name == "@deepseek-ai/dsh-mcp-client").unwrap();
        assert!(mcp.installed);
        assert_eq!(mcp.issue, "ok");
        let mkt = issues.iter().find(|i| i.name == "dshmarket").unwrap();
        assert!(!mkt.installed);
        assert_eq!(mkt.issue, "missing");
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn test_scan_and_clean_junk() {
        let work = std::env::temp_dir().join(format!("dshpm-junk-{}", std::process::id()));
        if work.exists() {
            let _ = std::fs::remove_dir_all(&work);
        }
        let profile = work.join("p");
        std::fs::create_dir_all(profile.join(".generations")).unwrap();
        std::fs::create_dir_all(profile.join("node_modules").join(".cache")).unwrap();
        std::fs::write(profile.join(".generations").join("x.bin"), vec![0u8; 2048]).unwrap();

        let junk = scan_junk(&profile);
        assert!(junk.iter().any(|j| j.name == ".generations"));
        assert!(junk.iter().any(|j| j.name == "node_modules/.cache"));
        let gen = junk.iter().find(|j| j.name == ".generations").unwrap();
        assert_eq!(gen.size, 2048);

        let removed = clean_junk(&profile, &["generations".to_string()]).unwrap();
        assert_eq!(removed, vec!["generations"]);
        assert!(!profile.join(".generations").exists());
        assert!(profile.join("node_modules").join(".cache").exists());

        // 非法目标拒绝
        assert!(clean_junk(&profile, &["..".to_string()]).is_err());
        let _ = std::fs::remove_dir_all(&work);
    }
}
