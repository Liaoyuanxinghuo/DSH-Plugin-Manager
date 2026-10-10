//! 插件补全：源 profile 清单读取 + 文件级复制（不断网、保留精确版本）
//!
//! - `read_manifest`：只读 profile/package.json 的 dependencies + dsh.profile.bundles
//! - `copy_plugins`：拷 node_modules/<pkg> 目录 + 合并目标 package.json
//!   + 补 pnpm-workspace.yaml；目标 patch 不动（新插件默认启用）
//! - 同名（包名精确匹配）由调用方做差集，这里只做单项幂等：目标已含则 skipped
//! - 基础组合包（inbox）拒绝复制

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// 基础组合包：不可补全（动了 profile 直接起不来）
pub const INBOX_BUNDLES: [&str; 3] = [
    "@deepseek-ai/dsh-base",
    "@deepseek-ai/dsh-web-app",
    "@deepseek-ai/dsh-headless",
];

/// profile 清单快照（补全差集用）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileManifest {
    pub dependencies: HashMap<String, String>,
    pub bundles: Vec<String>,
}

/// 只读 profile/package.json，返回清单快照
pub fn read_manifest(profile_dir: &Path) -> Result<ProfileManifest, String> {
    let pkg_path = profile_dir.join("package.json");
    let raw = std::fs::read_to_string(&pkg_path)
        .map_err(|e| format!("读取 package.json 失败: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("解析 package.json 失败: {e}"))?;
    let dependencies = v
        .get("dependencies")
        .and_then(|d| d.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, val)| (k.clone(), val.as_str().unwrap_or("").to_string()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let bundles = v
        .get("dsh")
        .and_then(|d| d.get("profile"))
        .and_then(|p| p.get("bundles"))
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(ProfileManifest {
        dependencies,
        bundles,
    })
}

/// 单项复制结果（前端按 ok/skipped 展示，失败带原因并继续）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyItemResult {
    pub name: String,
    pub ok: bool,
    pub skipped: bool,
    pub reason: String,
}

/// 递归复制单个包目录（src_pkg → dst_pkg）；符号链接跳过并计数
fn copy_pkg_dir(src_pkg: &Path, dst_pkg: &Path) -> Result<(usize, usize), String> {
    if !src_pkg.is_dir() {
        return Err("源包实体不存在".to_string());
    }
    // 必须含 package.json 才是完整包实体
    if !src_pkg.join("package.json").is_file() {
        return Err("源包实体不完整（缺 package.json），需联网安装".to_string());
    }
    let mut files = 0usize;
    let mut skipped = 0usize;
    copy_dir_inner(src_pkg, dst_pkg, &mut files, &mut skipped)
        .map_err(|e| format!("复制包文件失败: {e}"))?;
    if files == 0 {
        return Err("源包目录为空或不可读".to_string());
    }
    Ok((files, skipped))
}

fn copy_dir_inner(
    src: &Path,
    dst: &Path,
    files: &mut usize,
    skipped: &mut usize,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_inner(&from, &to, files, skipped)?;
        } else if ty.is_symlink() {
            *skipped += 1; // junction / 符号链接不跟随
        } else {
            match std::fs::copy(&from, &to) {
                Ok(_) => *files += 1,
                Err(e)
                    if e.kind() == std::io::ErrorKind::PermissionDenied
                        || e.raw_os_error() == Some(5) =>
                {
                    *skipped += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

/// 确保目标 profile 有 pnpm-workspace.yaml（dsh 模块解析依赖它）
fn ensure_workspace(profile_dir: &Path) -> Result<(), String> {
    let ws = profile_dir.join("pnpm-workspace.yaml");
    if ws.is_file() {
        return Ok(());
    }
    std::fs::write(
        &ws,
        "packages:\n  - .\n\nnodeLinker: hoisted\nautoInstallPeers: false\n",
    )
    .map_err(|e| format!("写入 pnpm-workspace.yaml 失败: {e}"))?;
    Ok(())
}

/// 文件级复制多个插件：拷实体 + 合并清单。单项失败不中断整体。
pub fn copy_plugins(
    src_dir: &Path,
    dst_dir: &Path,
    names: &[String],
) -> Result<Vec<CopyItemResult>, String> {
    if !src_dir.is_dir() {
        return Err("源 profile 目录不存在".to_string());
    }
    if !dst_dir.is_dir() {
        return Err("目标 profile 目录不存在".to_string());
    }
    // 规范化比较：防自复制
    let same = src_dir.canonicalize().ok().zip(dst_dir.canonicalize().ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false);
    if same {
        return Err("源与目标是同一目录，无需补全".to_string());
    }
    let src_man = read_manifest(src_dir)?;
    let mut out: Vec<CopyItemResult> = Vec::new();
    for name in names {
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        if INBOX_BUNDLES.contains(&name.as_str()) {
            out.push(CopyItemResult {
                name,
                ok: false,
                skipped: true,
                reason: "基础组合包，不参与补全".to_string(),
            });
            continue;
        }
        let Some(spec) = src_man.dependencies.get(&name).cloned() else {
            out.push(CopyItemResult {
                name,
                ok: false,
                skipped: true,
                reason: "源 profile 无此插件，已跳过".to_string(),
            });
            continue;
        };
        // 读目标清单（每次重读，保证多项连续写入不丢）
        let dst_raw = std::fs::read_to_string(dst_dir.join("package.json"))
            .map_err(|e| format!("读取目标 package.json 失败: {e}"))?;
        let mut dst_json: serde_json::Value = serde_json::from_str(&dst_raw)
            .map_err(|e| format!("解析目标 package.json 失败: {e}"))?;
        let has = dst_json
            .get("dependencies")
            .and_then(|d| d.as_object())
            .map(|m| m.contains_key(&name))
            .unwrap_or(false);
        if has {
            out.push(CopyItemResult {
                name,
                ok: false,
                skipped: true,
                reason: "目标已含同名插件，已跳过（版本不同也不覆盖）".to_string(),
            });
            continue;
        }
        // 1) 拷实体
        let src_pkg = src_dir.join("node_modules").join(&name);
        let dst_pkg = dst_dir.join("node_modules").join(&name);
        if let Some(parent) = dst_pkg.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建 node_modules 失败: {e}"))?;
        }
        match copy_pkg_dir(&src_pkg, &dst_pkg) {
            Ok(_) => {}
            Err(e) => {
                out.push(CopyItemResult {
                    name,
                    ok: false,
                    skipped: false,
                    reason: format!("{e}"),
                });
                continue;
            }
        }
        // 2) 合并清单：dependencies + bundles（原子写：临时文件 + rename）
        let is_bundle = src_man.bundles.iter().any(|b| b == &name);
        if let Some(deps) = dst_json
            .get_mut("dependencies")
            .and_then(|d| d.as_object_mut())
        {
            deps.insert(name.clone(), serde_json::Value::String(spec));
        } else if let Some(obj) = dst_json.as_object_mut() {
            let mut map = serde_json::Map::new();
            map.insert(name.clone(), serde_json::Value::String(spec));
            obj.insert("dependencies".to_string(), serde_json::Value::Object(map));
        }
        if is_bundle {
            // 确保 dsh.profile.bundles 数组存在并追加
            let bundles = dst_json
                .get_mut("dsh")
                .and_then(|d| d.get_mut("profile"))
                .and_then(|p| p.get_mut("bundles"))
                .and_then(|b| b.as_array_mut());
            if let Some(arr) = bundles {
                if !arr.iter().any(|v| v.as_str() == Some(&name)) {
                    arr.push(serde_json::Value::String(name.clone()));
                }
            }
        }
        let pretty = serde_json::to_string_pretty(&dst_json)
            .map_err(|e| format!("序列化 package.json 失败: {e}"))?;
        let tmp = dst_dir.join("package.json.dshpm-tmp");
        std::fs::write(&tmp, pretty + "\n")
            .map_err(|e| format!("写入临时清单失败: {e}"))?;
        if let Err(e) = std::fs::rename(&tmp, dst_dir.join("package.json")) {
            let _ = std::fs::remove_file(&tmp);
            // 回滚已拷实体，避免幽灵包
            let _ = std::fs::remove_dir_all(&dst_pkg);
            out.push(CopyItemResult {
                name,
                ok: false,
                skipped: false,
                reason: format!("替换 package.json 失败: {e}"),
            });
            continue;
        }
        if let Err(e) = ensure_workspace(dst_dir) {
            out.push(CopyItemResult {
                name,
                ok: true,
                skipped: false,
                reason: format!("已复制，但补 workspace 失败: {e}"),
            });
            continue;
        }
        out.push(CopyItemResult {
            name,
            ok: true,
            skipped: false,
            reason: String::new(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dshpm-cmp-{tag}-{}", std::process::id()));
        if d.exists() {
            let _ = std::fs::remove_dir_all(&d);
        }
        d
    }

    fn make_profile(dir: &std::path::Path, deps: &[(&str, &str)], bundles: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        let mut dep_map = serde_json::Map::new();
        for (k, v) in deps {
            dep_map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
        let pkg = serde_json::json!({
            "name": "dsh-profile-t",
            "private": true,
            "dependencies": dep_map,
            "dsh": { "profile": { "bundles": bundles } }
        });
        std::fs::write(dir.join("package.json"), serde_json::to_string_pretty(&pkg).unwrap()).unwrap();
    }

    fn make_pkg(profile: &std::path::Path, name: &str) {
        let pkg_dir = profile.join("node_modules").join(name);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(
            pkg_dir.join("package.json"),
            format!("{{\"name\":\"{name}\",\"version\":\"1.0.0\"}}"),
        )
        .unwrap();
        std::fs::write(pkg_dir.join("index.js"), "module.exports={}").unwrap();
    }

    #[test]
    fn read_manifest_parses_deps_and_bundles() {
        let work = tmpdir("read");
        let p = work.join("src");
        make_profile(&p, &[("aaa", "^1.0.0"), ("bbb", "2.0.0")], &["aaa"]);
        let m = read_manifest(&p).unwrap();
        assert_eq!(m.dependencies.get("aaa").map(String::as_str), Some("^1.0.0"));
        assert_eq!(m.bundles, vec!["aaa".to_string()]);
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn copy_skips_existing_and_inbox() {
        let work = tmpdir("skip");
        let src = work.join("src");
        let dst = work.join("dst");
        make_profile(&src, &[("keep", "1.0.0"), ("newp", "1.0.0")], &[]);
        make_profile(&dst, &[("keep", "9.9.9")], &[]);
        make_pkg(&src, "newp");
        make_pkg(&src, "keep");
        let r = copy_plugins(&src, &dst, &["keep".into(), "@deepseek-ai/dsh-base".into(), "newp".into()]).unwrap();
        assert_eq!(r.len(), 3);
        // keep：目标已有同名 → skipped（版本不同也不覆盖）
        let keep = r.iter().find(|x| x.name == "keep").unwrap();
        assert!(!keep.ok && keep.skipped);
        // inbox → skipped
        let inbox = r.iter().find(|x| x.name == "@deepseek-ai/dsh-base").unwrap();
        assert!(inbox.skipped);
        // newp → ok，且目标 spec 取源精确值
        let np = r.iter().find(|x| x.name == "newp").unwrap();
        assert!(np.ok);
        let m = read_manifest(&dst).unwrap();
        assert_eq!(m.dependencies.get("newp").map(String::as_str), Some("1.0.0"));
        assert!(dst.join("node_modules").join("newp").join("package.json").is_file());
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn copy_bundles_merged_and_pkg_without_entity_fails() {
        let work = tmpdir("bundle");
        let src = work.join("src");
        let dst = work.join("dst");
        make_profile(&src, &[("myb", "1.2.3")], &["myb"]);
        make_profile(&dst, &[], &[]);
        make_pkg(&src, "myb");
        // 缺实体的包 → 失败项（需联网安装）
        let r = copy_plugins(&src, &dst, &["myb".into(), "ghost".into()]).unwrap();
        let myb = r.iter().find(|x| x.name == "myb").unwrap();
        assert!(myb.ok);
        let m = read_manifest(&dst).unwrap();
        assert!(m.bundles.contains(&"myb".to_string()));
        let ghost = r.iter().find(|x| x.name == "ghost").unwrap();
        assert!(!ghost.ok);
        // 源有清单无实体 → 失败
        let src2 = work.join("src2");
        make_profile(&src2, &[("noent", "1.0.0")], &[]);
        let r2 = copy_plugins(&src2, &dst, &["noent".into()]).unwrap();
        assert!(!r2[0].ok);
        assert!(r2[0].reason.contains("联网") || r2[0].reason.contains("实体"));
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn copy_rejects_same_dir() {
        let work = tmpdir("same");
        let p = work.join("p");
        make_profile(&p, &[("a", "1.0.0")], &[]);
        assert!(copy_plugins(&p, &p, &["a".into()]).is_err());
        let _ = std::fs::remove_dir_all(&work);
    }
}
