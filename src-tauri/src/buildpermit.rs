//! pnpm 构建脚本白名单修复（pnpm 10 onlyBuiltDependencies 机制）
//! git 源插件（github:...）安装时需要执行 prepare 脚本，pnpm 默认阻止，
//! 需在 profile 的 pnpm-workspace.yaml 声明 onlyBuiltDependencies。

use std::path::Path;

/// 修复 pnpm-workspace.yaml：合并 onlyBuiltDependencies，返回最终列表
pub fn fix_build_permit(profile_dir: &Path, packages: &[String]) -> Result<Vec<String>, String> {
    let ws_path = profile_dir.join("pnpm-workspace.yaml");
    let mut names: Vec<String> = Vec::new();

    // 读取已有配置
    if ws_path.exists() {
        let raw = std::fs::read_to_string(&ws_path)
            .map_err(|e| format!("读取 pnpm-workspace.yaml 失败: {e}"))?;
        if let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(&raw) {
            if let Some(arr) = doc.get("onlyBuiltDependencies").and_then(|v| v.as_sequence()) {
                for v in arr {
                    if let Some(s) = v.as_str() {
                        names.push(s.to_string());
                    }
                }
            }
        }
    }

    // 合并新包（去重）
    let mut added = 0usize;
    for p in packages {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        if !names.iter().any(|n| n == p) {
            names.push(p.to_string());
            added += 1;
        }
    }
    if added == 0 {
        return Ok(names);
    }

    // 重建 yaml
    let mut doc = serde_yaml::Mapping::new();
    doc.insert(
        serde_yaml::Value::String("onlyBuiltDependencies".to_string()),
        serde_yaml::Value::Sequence(
            names.iter().map(|n| serde_yaml::Value::String(n.clone())).collect(),
        ),
    );
    let out = serde_yaml::to_string(&serde_yaml::Value::Mapping(doc))
        .map_err(|e| format!("序列化失败: {e}"))?;
    std::fs::write(&ws_path, out).map_err(|e| format!("写入 pnpm-workspace.yaml 失败: {e}"))?;
    Ok(names)
}

/// 从 pnpm 日志中提取需要允许构建的包名
/// 匹配形如: The git-hosted package "dshmarket@1.66.2" needs to execute build scripts
/// 前端也有等价实现（install 日志解析），此处保留供测试与后续命令使用
#[allow(dead_code)]
pub fn extract_build_packages(log: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in log.lines() {
        if line.contains("needs to execute build scripts")
            || line.contains("ERR_PNPM_GIT_DEP_PREPARE_NOT_ALLOWED")
            || line.contains("ERR_PNPM_RECURSIVE_BUILD_NOT_ALLOWED")
        {
            if let Some(start) = line.find('"') {
                let rest = &line[start + 1..];
                if let Some(end) = rest.find('"') {
                    let spec = &rest[..end];
                    let name = spec.split('@').next().unwrap_or(spec).to_string();
                    if !name.is_empty() && !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_build_packages() {
        let log = r#"
ERR_PNPM_GIT_DEP_PREPARE_NOT_ALLOWED Failed to prepare git-hosted package: The git-hosted package "dshmarket@1.66.2" needs to execute build scripts but is not in the "onlyBuiltDependencies" allowlist.
"#;
        let pkgs = extract_build_packages(log);
        assert_eq!(pkgs, vec!["dshmarket"]);
    }

    #[test]
    fn test_fix_build_permit_merge() {
        let work = std::env::temp_dir().join(format!("dshpm-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).unwrap();

        // 初始无配置
        let r1 = fix_build_permit(&work, &["dshmarket".to_string()]).unwrap();
        assert_eq!(r1, vec!["dshmarket"]);
        let ws = std::fs::read_to_string(work.join("pnpm-workspace.yaml")).unwrap();
        assert!(ws.contains("dshmarket"));

        // 再次合并不重复
        let r2 = fix_build_permit(&work, &["dshmarket".to_string(), "other".to_string()]).unwrap();
        assert_eq!(r2.len(), 2);
        assert!(r2.contains(&"dshmarket".to_string()));
        assert!(r2.contains(&"other".to_string()));

        // 无新增时不改写（返回原列表）
        let r3 = fix_build_permit(&work, &["dshmarket".to_string()]).unwrap();
        assert_eq!(r3.len(), 2);

        let _ = std::fs::remove_dir_all(&work);
    }
}
