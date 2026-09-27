//! 端到端验证：Profile 导出(压缩包) / 导入(重名改名) 方案可行性
//!
//! 使用用户本机真实 profile（优先取最小体积的）做完整链路验证：
//! 压缩 → 解压 → 重名改名导入 → 内容一致性校验 → 可解析性校验

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// 查找用于验证的真实 profile（优先 100KB~50MB 之间最小的，排除空/超大）
fn find_small_profile() -> Option<PathBuf> {
    let home = std::env::var("USERPROFILE").ok()?;
    let profiles = Path::new(&home).join(".dsh").join("profiles");
    let mut candidates: Vec<(u64, PathBuf)> = Vec::new();
    for entry in fs::read_dir(&profiles).ok()?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let size = dir_size(&path);
        if size > 100 * 1024 && size < 50 * 1024 * 1024 {
            candidates.push((size, path));
        }
    }
    candidates.sort_by_key(|(s, _)| *s);
    candidates.first().map(|(_, p)| p.clone())
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

/// 导出：profile 目录 → zip（zip 根 = profile 内容 + dshpm-manifest.json 记录原名）
fn export_profile(profile_dir: &Path, zip_path: &Path) -> Result<(usize, Vec<String>), String> {
    let file = fs::File::create(zip_path).map_err(|e| format!("创建 zip 失败: {e}"))?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut skips: Vec<String> = Vec::new();
    let mut count = 0usize;
    // 先写 manifest
    let manifest = serde_json::json!({
        "name": profile_dir.file_name().unwrap_or_default().to_string_lossy(),
        "format": "dshpm-profile",
        "version": 1,
    });
    zip.start_file("dshpm-manifest.json", options)
        .map_err(|e| format!("写 manifest 失败: {e}"))?;
    zip.write_all(manifest.to_string().as_bytes())
        .map_err(|e| format!("写 manifest 内容失败: {e}"))?;
    count += 1;
    // 再写 profile 内容
    add_dir_count(&mut zip, profile_dir, "", &options, &mut skips, &mut count)?;
    zip.finish().map_err(|e| format!("完成 zip 失败: {e}"))?;
    Ok((count, skips))
}

fn add_dir_count(
    zip: &mut ZipWriter<std::fs::File>,
    dir: &Path,
    prefix: &str,
    options: &SimpleFileOptions,
    skips: &mut Vec<String>,
    count: &mut usize,
) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("读取目录失败 {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
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
            add_dir_count(zip, &path, &zip_name, options, skips, count)?;
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

/// 导入：zip → 目标 profiles 目录（重名自动改名），返回最终目录名
fn import_profile(zip_path: &Path, target_profiles_dir: &Path) -> Result<String, String> {
    let file = fs::File::open(zip_path).map_err(|e| format!("打开 zip 失败: {e}"))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("解析 zip 失败: {e}"))?;

    // 先解压到临时目录
    let tmp = target_profiles_dir.join(".import-tmp");
    if tmp.exists() {
        fs::remove_dir_all(&tmp).map_err(|e| format!("清理临时目录失败: {e}"))?;
    }
    fs::create_dir_all(&tmp).map_err(|e| format!("创建临时目录失败: {e}"))?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("读取 zip 条目失败: {e}"))?;
        let out_path = tmp.join(entry.name());
        if entry.is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| format!("创建目录失败 {}: {e}", out_path.display()))?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("创建父目录失败: {e}"))?;
            }
            let mut out = fs::File::create(&out_path)
                .map_err(|e| format!("创建文件失败 {}: {e}", out_path.display()))?;
            std::io::copy(&mut entry, &mut out)
                .map_err(|e| format!("写出文件失败 {}: {e}", out_path.display()))?;
        }
    }

    // 校验是否合法 profile（含 package.json 且非空）
    let pkg = tmp.join("package.json");
    if !pkg.exists() {
        return Err("zip 内缺少 package.json，不是有效的 profile 备份".to_string());
    }
    let pkg_raw = fs::read_to_string(&pkg).map_err(|e| format!("读取 package.json 失败: {e}"))?;
    if pkg_raw.trim().is_empty() {
        return Err("package.json 为空，备份无效".to_string());
    }

    // 决定目标名称：优先 manifest 记录的原名，其次推断，最后 fallback
    let base_name = read_manifest_name(&mut archive)
        .or_else(|| infer_profile_name(&mut archive))
        .unwrap_or_else(|| "imported".to_string());

    // 重名处理：追加后缀直到不冲突
    let mut final_name = base_name.clone();
    let mut counter = 1;
    while target_profiles_dir.join(&final_name).exists() {
        final_name = format!("{base_name}-import-{counter}");
        counter += 1;
        if counter > 100 {
            return Err("无法找到不冲突的 profile 名称".to_string());
        }
    }

    // 移动临时目录 → 最终目录
    let dest = target_profiles_dir.join(&final_name);
    fs::rename(&tmp, &dest).map_err(|e| format!("移动目录失败: {e}"))?;
    Ok(final_name)
}

/// 读取 manifest 中的原名（若有）
fn read_manifest_name(archive: &mut ZipArchive<std::fs::File>) -> Option<String> {
    for i in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(i) else { continue };
        if entry.name() != "dshpm-manifest.json" {
            continue;
        }
        let mut buf = Vec::new();
        let _ = std::io::copy(&mut entry, &mut buf);
        let v: serde_json::Value = serde_json::from_slice(&buf).ok()?;
        return v.get("name").and_then(|n| n.as_str()).map(String::from);
    }
    None
}

/// 从 zip 推断 profile 名（只认顶层目录名，忽略顶层文件与常见内部目录）
fn infer_profile_name(archive: &mut ZipArchive<std::fs::File>) -> Option<String> {
    let mut candidate: Option<String> = None;
    for i in 0..archive.len() {
        let Ok(entry) = archive.by_index(i) else { continue };
        let name = entry.name().to_string();
        // 只考虑嵌套条目（顶层目录的成员）
        if !name.contains('/') {
            continue;
        }
        let top = name.split('/').next().unwrap_or("").to_string();
        if top.is_empty() || top.starts_with('.') {
            continue;
        }
        // 跳过明显非 profile 根目录的内部目录
        if matches!(top.as_str(), "node_modules" | "data" | "sessions" | "logs" | ".pnpm") {
            continue;
        }
        candidate = Some(top);
        break;
    }
    candidate
}

#[test]
fn test_profile_export_import_roundtrip() {
    let Some(profile_dir) = find_small_profile() else {
        eprintln!("SKIP: 未找到可用真实 profile");
        return;
    };
    let name = profile_dir.file_name().unwrap().to_string_lossy().to_string();
    eprintln!("使用真实 profile: {name} ({})", profile_dir.display());

    // 临时工作区
    let work = std::env::temp_dir().join(format!("dshpm-verify-{}", std::process::id()));
    if work.exists() {
        let _ = fs::remove_dir_all(&work);
    }
    fs::create_dir_all(&work).unwrap();
    let zip_path = work.join(format!("{name}.zip"));
    let fake_home = work.join("fake-home");
    let fake_profiles = fake_home.join("profiles");
    fs::create_dir_all(&fake_profiles).unwrap();

    // 步骤1：压缩
    let (files, skips) = export_profile(&profile_dir, &zip_path).expect("导出失败");
    let zip_size = fs::metadata(&zip_path).unwrap().len();
    eprintln!("导出完成: {files} 个文件, zip 大小 {} KB, 跳过 symlink {} 个", zip_size / 1024, skips.len());
    assert!(zip_size > 0, "zip 不应为空");
    // pnpm 安装的 node_modules 可能含 symlink（跳过是正常设计），不因 symlink 数量失败
    eprintln!("跳过 symlink {} 个", skips.len());

    // 步骤2：模拟重名场景 —— 先在目标目录放一个同名目录
    let conflict = fake_profiles.join(&name);
    fs::create_dir_all(&conflict).unwrap();
    fs::write(conflict.join("occupied.txt"), "i am here").unwrap();

    // 步骤3：导入（应自动改名）
    let imported = import_profile(&zip_path, &fake_profiles).expect("导入失败");
    assert_ne!(imported, name, "重名时应自动改名");
    assert!(imported.contains("import"), "改名应包含 import 标记，实际 {imported}");

    // 步骤4：校验完整性 —— 解压后的 package.json 与原 profile 一致
    let orig_pkg = fs::read_to_string(profile_dir.join("package.json")).unwrap();
    let new_pkg = fs::read_to_string(fake_profiles.join(&imported).join("package.json")).unwrap();
    assert_eq!(orig_pkg, new_pkg, "package.json 内容应完全一致");

    // 步骤5：可解析性 —— 用 scanner 逻辑解析（bundles + dependencies）
    let pkg_json: serde_json::Value = serde_json::from_str(&new_pkg).unwrap();
    let deps = pkg_json.get("dependencies").and_then(|d| d.as_object()).map(|m| m.len()).unwrap_or(0);
    eprintln!("导入成功: {imported}，可解析依赖数 {deps}");
    assert!(deps >= 1, "导入后的 profile 应至少解析出 1 个依赖");

    // 清理
    let _ = fs::remove_dir_all(&work);
    eprintln!("VERIFY OK: 导出→导入→改名→校验 全链路通过");
}

#[test]
fn test_export_with_chinese_filename() {
    // 验证 zip 对中文文件名支持（profile 内容层含中文文件名）
    let work = std::env::temp_dir().join(format!("dshpm-cn-{}", std::process::id()));
    if work.exists() {
        let _ = fs::remove_dir_all(&work);
    }
    let src = work.join("backup-profile");
    fs::create_dir_all(src.join("data")).unwrap();
    fs::write(src.join("data").join("会话记录.md"), "# 测试内容\n中文文本").unwrap();
    let zip_path = work.join("cn.zip");
    let (files, _) = export_profile(&src, &zip_path).expect("导出中文目录失败");
    assert!(files > 0);

    let out = work.join("out");
    fs::create_dir_all(&out).unwrap();
    let file = fs::File::open(&zip_path).unwrap();
    let mut archive = ZipArchive::new(file).unwrap();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let out_path = out.join(entry.name());
        if entry.is_dir() {
            fs::create_dir_all(&out_path).unwrap();
        } else {
            fs::create_dir_all(out_path.parent().unwrap()).unwrap();
            let mut f = fs::File::create(&out_path).unwrap();
            std::io::copy(&mut entry, &mut f).unwrap();
        }
    }
    // 中文文件名无损恢复
    let restored = fs::read_to_string(out.join("data").join("会话记录.md")).unwrap();
    assert!(restored.contains("中文文本"), "中文内容应无损恢复");
    // manifest 存在且记录原名
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(out.join("dshpm-manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["name"], "backup-profile");
    let _ = fs::remove_dir_all(&work);
    eprintln!("VERIFY OK: 中文文件名 + manifest 无损");
}
