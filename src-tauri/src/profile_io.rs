//! Profile 导出/导入：打包为 zip、校验、重名改名导入

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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

// ==================== 卸载收尾（market removeAndReconcile / dropFromManifest 等价） ====================

/// 半卸载修复：`dsh plugin remove` 可能在删掉 node_modules 之后、写回
/// package.json 之前失败（文件被占用中止）。磁盘上包已消失而清单还引用它时，
/// 下次启动会因幽灵依赖直接挂掉。按磁盘真相补全：删 dependencies 与
/// dsh.profile.bundles 里的残留行（market `dropFromManifest` 等价）。
/// 临时文件 + rename 原子写，避免半截清单。返回清单是否被改动。
pub fn drop_from_manifest(profile_dir: &Path, name: &str) -> Result<bool, String> {
    let file = profile_dir.join("package.json");
    let raw = fs::read_to_string(&file).map_err(|e| format!("读取 package.json 失败: {e}"))?;
    let mut pj: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("解析 package.json 失败: {e}"))?;
    let mut touched = false;
    if let Some(deps) = pj.get_mut("dependencies").and_then(|d| d.as_object_mut()) {
        if deps.remove(name).is_some() {
            touched = true;
        }
    }
    if let Some(bundles) = pj
        .get_mut("dsh")
        .and_then(|d| d.get_mut("profile"))
        .and_then(|p| p.get_mut("bundles"))
        .and_then(|b| b.as_array_mut())
    {
        let before = bundles.len();
        bundles.retain(|v| v.as_str() != Some(name));
        if bundles.len() != before {
            touched = true;
        }
    }
    if !touched {
        return Ok(false);
    }
    let out = serde_json::to_string_pretty(&pj).map_err(|e| format!("序列化失败: {e}"))?;
    let tmp = profile_dir.join("package.json.dshpm-tmp");
    fs::write(&tmp, out + "\n").map_err(|e| format!("写入临时清单失败: {e}"))?;
    fs::rename(&tmp, &file).map_err(|e| format!("替换 package.json 失败: {e}"))?;
    Ok(true)
}

/// host 部署目录对应的 node_modules 根（market `hostNodeModulesRoot` 等价）：
/// CLI 布局 `<prefix>/node_modules/@deepseek-ai/dsh` → `<prefix>/node_modules`；
/// 平铺布局（Desktop）→ `<dir>/node_modules`。
pub fn host_node_modules_root(host_dir: &Path) -> PathBuf {
    let p = host_dir;
    let is_cli_layout = p.file_name().map(|n| n == "dsh").unwrap_or(false)
        && p.parent()
            .and_then(|x| x.file_name())
            .map(|n| n == "@deepseek-ai")
            .unwrap_or(false)
        && p
            .parent()
            .and_then(|x| x.parent())
            .and_then(|x| x.file_name())
            .map(|n| n.eq_ignore_ascii_case("node_modules"))
            .unwrap_or(false);
    if is_cli_layout {
        return p.parent().and_then(|x| x.parent()).unwrap_or(p).to_path_buf();
    }
    p.join("node_modules")
}

/// 归一化链接目标再比较（去 `\\?\` / `\??\` 设备前缀；Windows 大小写不敏感）
fn points_at_profile_package(target: &Path, expected: &Path) -> bool {
    fn normalize(p: &Path) -> PathBuf {
        let s = p.to_string_lossy();
        let s = s.strip_prefix(r"\\?\UNC\").map(|r| format!(r"\\{r}")).unwrap_or_else(|| s.to_string());
        let s = s
            .strip_prefix(r"\\?\")
            .or_else(|| s.strip_prefix(r"\??\"))
            .unwrap_or(&s)
            .to_string();
        PathBuf::from(s)
    }
    let left = normalize(target);
    let right = normalize(expected);
    if cfg!(windows) {
        left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
    } else {
        left == right
    }
}

/// 官方启动会把 profile 的包以链接（Junction/Symlink）投射到 host 部署的
/// node_modules，且从不回收；`dsh plugin remove` 也不知道它们的存在。
/// 不清理的话坏链接会让 rg 等按 lstat 遍历的工具当场失败（market #662）。
/// 只在「host 侧入口是链接、指向本 profile 的该包副本、且副本确已消失」时删除。
/// 返回是否删掉了悬空桥。
pub fn remove_dangling_host_bridge(host_dir: Option<&Path>, profile_dir: &Path, name: &str) -> bool {
    let Some(host) = host_dir else {
        return false;
    };
    let bridge = host_node_modules_root(host).join(name);
    let unlinked = profile_dir.join("node_modules").join(name);
    // 包还在磁盘上 → 桥仍有效，绝不动
    if unlinked.join("package.json").exists() {
        return false;
    }
    let Ok(md) = fs::symlink_metadata(&bridge) else {
        return false;
    };
    if !md.file_type().is_symlink() {
        return false;
    }
    let Ok(target) = fs::read_link(&bridge) else {
        return false;
    };
    if !points_at_profile_package(&target, &unlinked) {
        return false;
    }
    // 目录链接（Junction/dir symlink）与文件链接的删除 API 不同；
    // symlink_metadata 的 is_dir() 对链接恒为 false，故两种都试。
    let removed = fs::remove_dir(&bridge).or_else(|_| fs::remove_file(&bridge));
    removed.is_ok()
}

/// 卸载收尾总入口：按磁盘真相补全 CLI 可能没做完的清理。
/// - CLI 成功：清单已由 CLI 调和，只清悬空桥 + 补丁行块
/// - CLI 失败但包已消失（半卸载）：补删清单行 + 悬空桥 + 补丁行块
/// - CLI 失败且包仍在：什么都不动（保留清单供重试）
/// `row_ids` 必须在删除**前**捕获（删除后包的 bundle patch 已读不到）。
/// 返回给用户看的收尾说明（可为空）。
pub fn reconcile_after_remove(
    profile_dir: &Path,
    host_dir: Option<&Path>,
    name: &str,
    row_ids: &[String],
    cli_ok: bool,
) -> String {
    let pkg_gone = !profile_dir
        .join("node_modules")
        .join(name)
        .join("package.json")
        .exists();
    if !cli_ok && !pkg_gone {
        return "卸载失败且插件仍在磁盘，清单已保留，可重试".to_string();
    }
    let mut notes: Vec<String> = Vec::new();
    if pkg_gone {
        match drop_from_manifest(profile_dir, name) {
            Ok(true) => notes.push("已按磁盘真相补全 package.json 清理".to_string()),
            Ok(false) => {}
            Err(e) => notes.push(format!("package.json 清理失败: {e}")),
        }
        if remove_dangling_host_bridge(host_dir, profile_dir, name) {
            notes.push("已清理 host node_modules 悬空桥接链接".to_string());
        }
    }
    // 行块清理：CLI 成功或半卸载都要做，卸载后不应留 orphan disabled 行
    let patch_path = profile_dir.join("cordis.patch.yml");
    if let Ok(content) = fs::read_to_string(&patch_path) {
        let next = crate::patchfile::remove_row_blocks(&content, row_ids);
        if next != content {
            if let Err(e) = fs::write(&patch_path, &next) {
                notes.push(format!("补丁行清理失败: {e}"));
            } else {
                notes.push("已清理 cordis.patch.yml 中的残留停用行".to_string());
            }
        }
    }
    if !cli_ok {
        notes.insert(
            0,
            "CLI 未正常结束但插件已从磁盘消失，已按磁盘真相完成卸载".to_string(),
        );
    }
    notes.join("；")
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

    // ===== 卸载收尾：drop_from_manifest / 悬空桥 / reconcile =====

    #[test]
    fn test_drop_from_manifest_removes_dep_and_bundle() {
        let work = std::env::temp_dir().join(format!("dshpm-dropman-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).unwrap();
        fs::write(
            work.join("package.json"),
            r#"{"name":"p","dependencies":{"ghost":"1.0.0","keep":"2.0.0"},"dsh":{"profile":{"bundles":["keep","ghost"]}}}"#,
        )
        .unwrap();
        assert!(drop_from_manifest(&work, "ghost").unwrap());
        let raw = fs::read_to_string(work.join("package.json")).unwrap();
        let pj: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(pj["dependencies"].get("ghost").is_none(), "{raw}");
        assert_eq!(pj["dependencies"]["keep"], "2.0.0");
        let bundles = pj["dsh"]["profile"]["bundles"].as_array().unwrap();
        assert_eq!(bundles.len(), 1);
        assert_eq!(bundles[0], "keep");
        // 幂等：第二次无事可做
        assert!(!drop_from_manifest(&work, "ghost").unwrap());
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn test_host_node_modules_root_layouts() {
        // CLI 布局：<prefix>/node_modules/@deepseek-ai/dsh → <prefix>/node_modules
        let cli = PathBuf::from(r"C:\dsh-versions\dsh-0.2.0-rc.1\node_modules\@deepseek-ai\dsh");
        assert_eq!(
            host_node_modules_root(&cli),
            PathBuf::from(r"C:\dsh-versions\dsh-0.2.0-rc.1\node_modules")
        );
        // 平铺布局：<dir> → <dir>/node_modules
        let flat = PathBuf::from(r"C:\app\dependencies\dsh");
        assert_eq!(
            host_node_modules_root(&flat),
            PathBuf::from(r"C:\app\dependencies\dsh\node_modules")
        );
    }

    /// 系统级功能测试：真实 Junction/目录符号链接的悬空桥清理
    #[test]
    fn test_remove_dangling_host_bridge() {
        let work = std::env::temp_dir().join(format!("dshpm-bridge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        let profile = work.join("profiles").join("p1");
        let host = work.join("host");
        let pkg = profile.join("node_modules").join("my-plugin");
        let host_nm = host.join("node_modules");
        fs::create_dir_all(&pkg).unwrap();
        fs::create_dir_all(&host_nm).unwrap();
        fs::write(pkg.join("package.json"), "{}").unwrap();

        // 桥 = host node_modules 下指向 profile 包副本的目录符号链接
        let bridge = host_nm.join("my-plugin");
        std::os::windows::fs::symlink_dir(&pkg, &bridge).unwrap();

        // 包还在 → 桥有效，不动
        assert!(!remove_dangling_host_bridge(Some(&host), &profile, "my-plugin"));
        assert!(bridge.exists() || fs::symlink_metadata(&bridge).is_ok());

        // 包消失（半卸载）→ 悬空桥删除
        fs::remove_file(pkg.join("package.json")).unwrap();
        assert!(remove_dangling_host_bridge(Some(&host), &profile, "my-plugin"));
        assert!(fs::symlink_metadata(&bridge).is_err(), "悬空桥应被删除");

        // 指向别处的链接不删
        let other = work.join("elsewhere");
        fs::create_dir_all(&other).unwrap();
        std::os::windows::fs::symlink_dir(&other, &bridge).unwrap();
        assert!(!remove_dangling_host_bridge(Some(&host), &profile, "my-plugin"));
        assert!(fs::symlink_metadata(&bridge).is_ok(), "他人链接必须保留");

        // host 缺失 → no-op
        assert!(!remove_dangling_host_bridge(None, &profile, "my-plugin"));
        let _ = fs::remove_dir_all(&work);
    }

    /// 系统级功能测试：半卸载（CLI 失败但包已消失）→ 补全清单 + 行块清理
    #[test]
    fn test_reconcile_after_remove_half_uninstall() {
        let work = std::env::temp_dir().join(format!("dshpm-recon-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        let profile = work.join("profiles").join("p1");
        fs::create_dir_all(profile.join("node_modules")).unwrap();
        fs::write(
            profile.join("package.json"),
            r#"{"name":"p","dependencies":{"ghost":"1.0.0"},"dsh":{"profile":{"bundles":["ghost"]}}}"#,
        )
        .unwrap();
        fs::write(
            profile.join("cordis.patch.yml"),
            "- id: ghost\n  disabled: true\n- id: other\n  disabled: true\n",
        )
        .unwrap();

        // 半卸载：CLI 失败、包已不在磁盘
        let note = reconcile_after_remove(&profile, None, "ghost", &["ghost".to_string()], false);
        assert!(!note.is_empty(), "应有收尾说明");
        let raw = fs::read_to_string(profile.join("package.json")).unwrap();
        assert!(!raw.contains("ghost"), "幽灵依赖应被清掉: {raw}");
        let patch = fs::read_to_string(profile.join("cordis.patch.yml")).unwrap();
        assert!(!patch.contains("- id: ghost"), "orphan 行应被清掉: {patch}");
        assert!(patch.contains("- id: other"), "他人行保留: {patch}");

        // CLI 失败且包仍在 → 不动清单（可重试）
        fs::create_dir_all(profile.join("node_modules").join("stillhere")).unwrap();
        fs::write(profile.join("node_modules").join("stillhere").join("package.json"), "{}").unwrap();
        fs::write(
            profile.join("package.json"),
            r#"{"name":"p","dependencies":{"stillhere":"1.0.0"}}"#,
        )
        .unwrap();
        let note2 = reconcile_after_remove(&profile, None, "stillhere", &[], false);
        assert!(note2.contains("重试"), "{note2}");
        let raw2 = fs::read_to_string(profile.join("package.json")).unwrap();
        assert!(raw2.contains("stillhere"), "失败且仍在磁盘时不得动清单: {raw2}");

        let _ = fs::remove_dir_all(&work);
    }
}
