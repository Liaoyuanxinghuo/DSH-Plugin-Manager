//! Profile 导出/导入：打包为 zip、校验、重名改名导入

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// 导出结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub file_count: usize,
    pub zip_size: u64,
    pub skipped_symlinks: Vec<String>,
    pub target_path: String,
}

/// 导入结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub profile_name: String,
    pub final_name: String,
    pub renamed: bool,
    pub plugin_count: usize,
    pub target_path: String,
}

const MANIFEST_NAME: &str = "dshpm-manifest.json";

/// 递归把目录写入 zip（跳过 symlink，跳过临时/输出文件）
fn add_dir(
    zip: &mut ZipWriter<fs::File>,
    dir: &Path,
    prefix: &str,
    options: &SimpleFileOptions,
    skips: &mut Vec<String>,
    count: &mut usize,
    excludes: &[String],
) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("读取目录失败 {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // 排除指定项
        if excludes.iter().any(|x| x == &name) {
            continue;
        }
        let zip_name = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let ft = fs::symlink_metadata(&path)
            .map(|m| m.file_type())
            .map_err(|e| format!("metadata 失败 {}: {e}", path.display()))?;
        if ft.is_symlink() {
            skips.push(zip_name);
            continue;
        }
        if path.is_dir() {
            zip.add_directory(&zip_name, *options)
                .map_err(|e| format!("添加目录失败 {zip_name}: {e}"))?;
            add_dir(zip, &path, &zip_name, options, skips, count, excludes)?;
        } else {
            zip.start_file(&zip_name, *options)
                .map_err(|e| format!("添加文件失败 {zip_name}: {e}"))?;
            let data = fs::read(&path).map_err(|e| format!("读取文件失败 {}: {e}", path.display()))?;
            zip.write_all(&data)
                .map_err(|e| format!("写入文件失败 {zip_name}: {e}"))?;
            *count += 1;
        }
    }
    Ok(())
}

/// 导出 profile 目录为 zip
pub fn export_profile(
    profile_dir: &Path,
    zip_path: &Path,
    exclude_node_modules: bool,
) -> Result<ExportResult, String> {
    let file = fs::File::create(zip_path).map_err(|e| format!("创建 zip 失败: {e}"))?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut skips: Vec<String> = Vec::new();
    let mut count = 0usize;

    // 写 manifest 记录原名
    let manifest = serde_json::json!({
        "name": profile_dir.file_name().unwrap_or_default().to_string_lossy(),
        "format": "dshpm-profile",
        "version": 1,
    });
    zip.start_file(MANIFEST_NAME, options)
        .map_err(|e| format!("写 manifest 失败: {e}"))?;
    zip.write_all(manifest.to_string().as_bytes())
        .map_err(|e| format!("写 manifest 内容失败: {e}"))?;
    count += 1;

    // 排除项：临时目录
    let mut excludes: Vec<String> = vec![".import-tmp".to_string()];
    if exclude_node_modules {
        excludes.push("node_modules".to_string());
    }

    add_dir(&mut zip, profile_dir, "", &options, &mut skips, &mut count, &excludes)?;
    zip.finish().map_err(|e| format!("完成 zip 失败: {e}"))?;

    let zip_size = fs::metadata(zip_path)
        .map(|m| m.len())
        .unwrap_or(0);
    Ok(ExportResult {
        file_count: count,
        zip_size,
        skipped_symlinks: skips,
        target_path: zip_path.to_string_lossy().to_string(),
    })
}

/// 读取 manifest 中的原名
fn read_manifest_name(archive: &mut ZipArchive<fs::File>) -> Option<String> {
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else { continue };
        if entry.name() != MANIFEST_NAME {
            continue;
        }
        let mut buf = Vec::new();
        let _ = std::io::copy(&mut entry, &mut buf);
        let v: serde_json::Value = serde_json::from_slice(&buf).ok()?;
        return v.get("name").and_then(|n| n.as_str()).map(String::from);
    }
    None
}

/// 从 zip 推断 profile 名（顶层目录名；无 manifest 的外部 zip 兼容）
fn infer_profile_name(archive: &mut ZipArchive<fs::File>) -> Option<String> {
    let mut candidate: Option<String> = None;
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else { continue };
        let name = entry.name().to_string();
        if !name.contains('/') {
            continue;
        }
        let top = name.split('/').next().unwrap_or("").to_string();
        if top.is_empty() || top.starts_with('.') {
            continue;
        }
        if matches!(top.as_str(), "node_modules" | "data" | "sessions" | "logs" | ".pnpm") {
            continue;
        }
        candidate = Some(top);
        break;
    }
    candidate
}

/// 导入 zip 到目标 profiles 目录；重名自动改名
pub fn import_profile(zip_path: &Path, target_profiles_dir: &Path) -> Result<ImportResult, String> {
    let file = fs::File::open(zip_path).map_err(|e| format!("打开 zip 失败: {e}"))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("解析 zip 失败: {e}"))?;

    let tmp = target_profiles_dir.join(".import-tmp");
    if tmp.exists() {
        fs::remove_dir_all(&tmp).map_err(|e| format!("清理临时目录失败: {e}"))?;
    }
    fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败: {e}"))?;

    // 解压全部条目（防路径穿越）
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("读取 zip 条目失败: {e}"))?;
        let name = entry.name().to_string();
        // 安全校验：拒绝绝对路径与 .. 穿越
        let clean = Path::new(&name);
        if clean.is_absolute() || name.split('/').any(|seg| seg == "..") {
            return Err(format!("zip 包含非法路径: {name}"));
        }
        let out_path = tmp.join(&name);
        if entry.is_dir() {
            fs::create_dir_all(&out_path)
                .map_err(|e| format!("创建目录失败 {}: {e}", out_path.display()))?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("创建父目录失败 {}: {e}", parent.display()))?;
            }
            let mut out = fs::File::create(&out_path)
                .map_err(|e| format!("创建文件失败 {}: {e}", out_path.display()))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| format!("写出文件失败 {}: {e}", out_path.display()))?;
        }
    }

    // 校验合法性
    let pkg = tmp.join("package.json");
    if !pkg.exists() {
        let _ = fs::remove_dir_all(&tmp);
        return Err("zip 内缺少 package.json，不是有效的 profile 备份".to_string());
    }
    let pkg_raw = fs::read_to_string(&pkg).map_err(|e| format!("读取 package.json 失败: {e}"))?;
    if pkg_raw.trim().is_empty() {
        let _ = fs::remove_dir_all(&tmp);
        return Err("package.json 为空，备份无效".to_string());
    }
    // 解析插件数量
    let plugin_count = serde_json::from_str::<serde_json::Value>(&pkg_raw)
        .ok()
        .and_then(|v| {
            v.get("dependencies")
                .and_then(|d| d.as_object())
                .map(|m| m.len())
        })
        .unwrap_or(0);

    // 确定目标名：manifest → 推断 → fallback
    let base_name = read_manifest_name(&mut archive)
        .or_else(|| infer_profile_name(&mut archive))
        .unwrap_or_else(|| "imported".to_string());
    // 清洗不合规字符（Windows 文件名限制）
    let base_name = sanitize_name(&base_name);

    let mut final_name = base_name.clone();
    let mut counter = 1;
    while target_profiles_dir.join(&final_name).exists() {
        final_name = format!("{base_name}-import-{counter}");
        counter += 1;
        if counter > 100 {
            let _ = fs::remove_dir_all(&tmp);
            return Err("无法找到不冲突的 profile 名称".to_string());
        }
    }
    let renamed = final_name != base_name;

    let dest = target_profiles_dir.join(&final_name);
    fs::rename(&tmp, &dest).map_err(|e| format!("移动目录失败: {e}"))?;

    Ok(ImportResult {
        profile_name: base_name,
        final_name,
        renamed,
        plugin_count,
        target_path: dest.to_string_lossy().to_string(),
    })
}

/// 清洗 profile 名中 Windows 不合规字符
fn sanitize_name(name: &str) -> String {
    let invalid = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    let cleaned: String = name
        .chars()
        .map(|c| if invalid.contains(&c) { '_' } else { c })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').trim_end_matches(' ');
    if trimmed.is_empty() {
        "imported".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 导出诊断包：DSH_HOME 配置 + 会话/存储摘要 + 各 profile 元数据（不含 node_modules）
pub fn export_diag(home_dir: &Path, target_zip: &Path) -> Result<ExportResult, String> {
    if let Some(dir) = target_zip.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let file = fs::File::create(target_zip).map_err(|e| format!("创建压缩包失败: {e}"))?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);

    let mut count = 0usize;
    let mut skipped = Vec::new();

    // 顶层配置（排除 .credentials.yaml —— 含密钥，不进诊断包）
    if let Ok(entries) = fs::read_dir(home_dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name == ".credentials.yaml" || name == "profiles" || name == "sessions" || name == "storages" {
                continue;
            }
            let path = e.path();
            if path.is_file() {
                if let Ok(md) = fs::metadata(&path) {
                    if md.len() <= 1 * 1024 * 1024 {
                        zip.start_file(format!("dsh-home/{name}"), opts).map_err(|e| format!("写入压缩包失败: {e}"))?;
                        if let Ok(content) = fs::read(&path) {
                            zip.write_all(&content).map_err(|e| format!("写入压缩包失败: {e}"))?;
                            count += 1;
                        }
                    } else {
                        skipped.push(name);
                    }
                }
            }
        }
    }

    // profiles 元数据（每个 profile 的 package.json / 补丁 / 配置文件，不含 node_modules）
    let profiles_dir = home_dir.join("profiles");
    if profiles_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&profiles_dir) {
            for e in entries.flatten() {
                let pname = e.file_name().to_string_lossy().to_string();
                let pdir = e.path();
                if !pdir.is_dir() || pname.starts_with('.') || pname == "node_modules" {
                    continue;
                }
                for f in ["package.json", "cordis.patch.yml", "cordis.patch.yml.bak", "dsh.bundle.patch", "pnpm-workspace.yaml", ".npmrc"] {
                    let fpath = pdir.join(f);
                    if fpath.is_file() {
                        if let Ok(md) = fs::metadata(&fpath) {
                            if md.len() <= 512 * 1024 {
                                zip.start_file(format!("profiles/{pname}/{f}"), opts).map_err(|e| format!("写入压缩包失败: {e}"))?;
                                if let Ok(content) = fs::read(&fpath) {
                                    zip.write_all(&content).map_err(|e| format!("写入压缩包失败: {e}"))?;
                                    count += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // sessions / storages：目录清单 + 小文件（≤256KB）
    for area in ["sessions", "storages"] {
        let area_dir = home_dir.join(area);
        if !area_dir.is_dir() {
            continue;
        }
        add_area_summary(&mut zip, &area_dir, area, &opts, &mut count, &mut skipped)?;
    }

    zip.finish().map_err(|e| format!("压缩包写入失败: {e}"))?;
    let zip_size = fs::metadata(target_zip).map(|m| m.len()).unwrap_or(0);
    Ok(ExportResult {
        file_count: count,
        zip_size,
        skipped_symlinks: skipped,
        target_path: target_zip.to_string_lossy().to_string(),
    })
}

/// 打包某区域（sessions/storages）：目录清单 + 小文件
fn add_area_summary(
    zip: &mut ZipWriter<fs::File>,
    dir: &Path,
    prefix: &str,
    opts: &SimpleFileOptions,
    count: &mut usize,
    skipped: &mut Vec<String>,
) -> Result<(), String> {
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let path = e.path();
            if path.is_dir() {
                zip.start_file(format!("{prefix}/{name}/.dir"), *opts).map_err(|e| format!("写入压缩包失败: {e}"))?;
                zip.write_all(b"").map_err(|e| format!("写入压缩包失败: {e}"))?;
                *count += 1;
                if let Ok(inner) = fs::read_dir(&path) {
                    for ie in inner.flatten() {
                        let iname = ie.file_name().to_string_lossy().to_string();
                        let ipath = ie.path();
                        if ipath.is_file() {
                            if let Ok(md) = fs::metadata(&ipath) {
                                if md.len() <= 256 * 1024 {
                                    zip.start_file(format!("{prefix}/{name}/{iname}"), *opts).map_err(|e| format!("写入压缩包失败: {e}"))?;
                                    if let Ok(content) = fs::read(&ipath) {
                                        zip.write_all(&content).map_err(|e| format!("写入压缩包失败: {e}"))?;
                                        *count += 1;
                                    }
                                } else {
                                    skipped.push(format!("{prefix}/{name}/{iname}"));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// 新建 profile：创建目录 + 标准 package.json（含默认内置 bundle）
pub fn create_profile(profiles_dir: &Path, name: &str) -> Result<CreateProfileResult, String> {
    let safe = sanitize_name(name);
    // sanitize 会把 ".."/"." 等清洗为 "imported"，一并拒绝
    if safe == "." || safe == ".." || safe.is_empty() || safe == "imported" {
        return Err(format!("非法的 profile 名称: {name}"));
    }
    let target = profiles_dir.join(&safe);
    if target.exists() {
        return Err(format!("profile 已存在: {safe}"));
    }
    let pkg = serde_json::json!({
        "name": format!("dsh-profile-{safe}"),
        "private": true,
        "dependencies": {},
        "dsh": {
            "profile": {
                "bundles": ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"]
            }
        }
    });
    fs::create_dir_all(&target).map_err(|e| format!("创建目录失败: {e}"))?;
    let pkg_path = target.join("package.json");
    fs::write(&pkg_path, serde_json::to_string_pretty(&pkg).unwrap())
        .map_err(|e| format!("写入 package.json 失败: {e}"))?;
    // 与现有正常 profile 一致：声明独立 workspace，依赖安装到本 profile 的 node_modules
    fs::write(
        target.join("pnpm-workspace.yaml"),
        "packages:\n  - .\n\nnodeLinker: hoisted\nautoInstallPeers: false\n",
    )
    .map_err(|e| format!("写入 pnpm-workspace.yaml 失败: {e}"))?;
    // 对齐官方 initProfile：空用户补丁层（注释 + 空数组）
    fs::write(
        target.join("cordis.patch.yml"),
        "# Your patch layer for this dsh profile, applied after every bundle layer:\n# a top-level YAML array of loader patch entries (id-targeted config\n# overrides, disables, and insert lists; `!!js` expressions allowed).\n[]\n",
    )
    .map_err(|e| format!("写入 cordis.patch.yml 失败: {e}"))?;
    Ok(CreateProfileResult {
        name: safe,
        path: target.to_string_lossy().to_string(),
    })
}

/// 删除 profile（目录含 node_modules）。running=true 时拒绝。
pub fn delete_profile(profiles_dir: &Path, name: &str, running: bool) -> Result<(), String> {
    let safe = sanitize_name(name);
    if safe == "." || safe == ".." || safe.is_empty() || safe == "imported" {
        return Err(format!("非法的 profile 名称: {name}"));
    }
    let target = profiles_dir.join(&safe);
    if !target.is_dir() {
        return Err(format!("profile 不存在: {safe}"));
    }
    // 防路径穿越：确认目标仍在 profiles_dir 内
    let canon_profiles = profiles_dir.canonicalize().map_err(|e| format!("无法解析 profiles 目录: {e}"))?;
    let canon_target = target.canonicalize().map_err(|e| format!("无法解析目标: {e}"))?;
    if !canon_target.starts_with(&canon_profiles) {
        return Err(format!("目标不在 profiles 目录内: {safe}"));
    }
    if running {
        return Err(format!("profile「{safe}」正在运行，请先停止再删除"));
    }
    fs::remove_dir_all(&target).map_err(|e| format!("删除失败: {e}"))?;
    Ok(())
}

/// 新建结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProfileResult {
    pub name: String,
    pub path: String,
}

/// 列出 profile 内 node_modules 大小（用于导出前提示）
pub fn node_modules_size(profile_dir: &Path) -> u64 {
    let nm = profile_dir.join("node_modules");
    if !nm.is_dir() {
        return 0;
    }
    dir_size(&nm)
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(md) = fs::metadata(&p) {
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
    fn test_sanitize_name() {
        assert_eq!(sanitize_name("web"), "web");
        assert_eq!(sanitize_name("a<b>c:d\"e/f\\g|h?i*j"), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(sanitize_name("  "), "imported");
        assert_eq!(sanitize_name("."), "imported");
    }

    #[test]
    fn test_roundtrip_temp() {
        // 纯临时目录往返（不依赖真实 profile）
        let work = std::env::temp_dir().join(format!("dshpm-rt-{}", std::process::id()));
        if work.exists() {
            let _ = fs::remove_dir_all(&work);
        }
        let profile = work.join("profiles").join("my-test");
        fs::create_dir_all(profile.join("data")).unwrap();
        fs::write(profile.join("package.json"), r#"{"name":"p","dependencies":{"a":"1.0.0"}}"#).unwrap();
        fs::write(profile.join("data").join("s.md"), "中文内容").unwrap();

        let zip = work.join("out.zip");
        let exp = export_profile(&profile, &zip, false).unwrap();
        assert!(exp.file_count >= 3);

        let target = work.join("home2").join("profiles");
        fs::create_dir_all(&target).unwrap();
        // 制造重名
        fs::create_dir_all(target.join("my-test")).unwrap();
        let imp = import_profile(&zip, &target).unwrap();
        assert!(imp.renamed);
        assert!(imp.final_name.contains("import"));
        assert_eq!(imp.plugin_count, 1);
        let restored = fs::read_to_string(target.join(&imp.final_name).join("data").join("s.md")).unwrap();
        assert_eq!(restored, "中文内容");

        // 二次导入应继续改名
        let imp2 = import_profile(&zip, &target).unwrap();
        assert!(imp2.renamed);
        assert_ne!(imp2.final_name, imp.final_name);

        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn test_export_diag() {
        let work = std::env::temp_dir().join(format!("dshpm-diag-{}", std::process::id()));
        if work.exists() {
            let _ = fs::remove_dir_all(&work);
        }
        let home = work.join("dsh-home");
        fs::create_dir_all(home.join("profiles").join("web")).unwrap();
        fs::create_dir_all(home.join("sessions").join("s1")).unwrap();
        fs::write(home.join("settings.yaml.imported"), "theme: dark").unwrap();
        fs::write(home.join(".credentials.yaml"), "SECRET").unwrap();
        fs::write(home.join("profiles").join("web").join("package.json"), r#"{"name":"web"}"#).unwrap();
        fs::write(home.join("sessions").join("s1").join("meta.json"), "{}").unwrap();

        let zip_path = work.join("diag.zip");
        let r = export_diag(&home, &zip_path).unwrap();
        assert!(r.file_count >= 4, "应打包配置+package.json+会话，实际 {}", r.file_count);
        assert!(zip_path.exists());

        // credentials 不应进入诊断包
        let file = fs::File::open(&zip_path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len()).map(|i| archive.by_index(i).unwrap().name().to_string()).collect();
        assert!(!names.iter().any(|n| n.contains("credentials")), "诊断包不应含密钥");
        assert!(names.iter().any(|n| n.contains("package.json")));
        assert!(names.iter().any(|n| n.contains("settings.yaml.imported")));
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn test_create_and_delete_profile() {
        let work = std::env::temp_dir().join(format!("dshpm-cd-{}", std::process::id()));
        if work.exists() {
            let _ = fs::remove_dir_all(&work);
        }
        let profiles = work.join("profiles");
        fs::create_dir_all(&profiles).unwrap();

        // 新建
        let r = create_profile(&profiles, "my-new").unwrap();
        assert_eq!(r.name, "my-new");
        assert!(profiles.join("my-new").join("package.json").exists());
        let pkg_raw = fs::read_to_string(profiles.join("my-new").join("package.json")).unwrap();
        let pkg: serde_json::Value = serde_json::from_str(&pkg_raw).unwrap();
        assert_eq!(pkg["name"], "dsh-profile-my-new");
        assert!(pkg["dsh"]["profile"]["bundles"][0].as_str().unwrap().contains("dsh-base"));
        // 对齐官方 initProfile：三件套
        assert!(profiles.join("my-new").join("pnpm-workspace.yaml").exists());
        let patch = fs::read_to_string(profiles.join("my-new").join("cordis.patch.yml")).unwrap();
        assert!(patch.trim().ends_with(']'), "应写入空补丁层: {patch}");

        // 重名报错
        assert!(create_profile(&profiles, "my-new").is_err());
        // 非法名报错
        assert!(create_profile(&profiles, "..").is_err());
        assert!(create_profile(&profiles, "a<b>c").is_ok(), "特殊字符应被清洗后可用");

        // 删除
        delete_profile(&profiles, "my-new", false).unwrap();
        assert!(!profiles.join("my-new").exists());
        // 删除不存在的报错
        assert!(delete_profile(&profiles, "my-new", false).is_err());
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn test_delete_refuses_running() {
        let work = std::env::temp_dir().join(format!("dshpm-dr-{}", std::process::id()));
        if work.exists() {
            let _ = fs::remove_dir_all(&work);
        }
        let profiles = work.join("profiles");
        fs::create_dir_all(&profiles).unwrap();
        create_profile(&profiles, "busy").unwrap();
        let r = delete_profile(&profiles, "busy", true);
        assert!(r.is_err());
        assert!(profiles.join("busy").exists());
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn test_import_rejects_bad_zip() {
        // 缺 package.json 的 zip 应拒绝
        let work = std::env::temp_dir().join(format!("dshpm-bad-{}", std::process::id()));
        if work.exists() {
            let _ = fs::remove_dir_all(&work);
        }
        let src = work.join("bad");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("readme.txt"), "not a profile").unwrap();
        let zip = work.join("bad.zip");
        let f = fs::File::create(&zip).unwrap();
        let mut zw = ZipWriter::new(f);
        let opts = SimpleFileOptions::default();
        zw.start_file("readme.txt", opts).unwrap();
        zw.write_all(b"not a profile").unwrap();
        zw.finish().unwrap();

        let target = work.join("profiles");
        fs::create_dir_all(&target).unwrap();
        let r = import_profile(&zip, &target);
        assert!(r.is_err());
        let _ = fs::remove_dir_all(&work);
    }
}
