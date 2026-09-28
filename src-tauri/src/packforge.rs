//! DSH-PackForge 整合包（.dspack v3 / manifest v5）导出、市场、下载、导入
//! 契约来源：https://github.com/DSH-PackForge/DSH-PackForge
//! - pack-structure v3：ZIP 根含 dspack.json（{format:"dspack", version:3}）+ manifest.json（v5）
//! - profile 形态：overrides/ → profile 根，可选 home/ → $DSH_HOME
//! - 安全过滤：node_modules/凭据/压缩包等一律不进包

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// 市场索引契约（index.json schemaVersion 2）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketPackEntry {
    pub name: String,
    pub version: String,
    /// 展示名（字符串或多语言 map 的 zh-CN 优先解析结果）
    pub display_name: String,
    pub description: String,
    pub author: String,
    pub category: String,
    pub dsh_version: String,
    pub profile_name: String,
    pub download_url: String,
    pub sha256: String,
    pub size: u64,
    pub updated_at: String,
    pub id: String,
    pub owner: String,
    pub repo: String,
    pub bundle_count: u32,
    pub dep_count: u32,
    pub profile_count: u32,
    pub manifest_version: u32,
    pub pack_type: String,
}

#[derive(Deserialize)]
struct MarketIndex {
    #[serde(default)]
    modpacks: Vec<serde_json::Value>,
}

/// 导出结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackExportResult {
    pub file_count: usize,
    pub zip_size: u64,
    pub sha256: String,
    pub target_path: String,
}

/// 导入结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackImportResult {
    pub pack_name: String,
    pub pack_version: String,
    pub pack_type: String,
    /// 落盘的 profile 名（profile 形态 1 个，dshhome 形态多个）
    pub final_names: Vec<String>,
    pub renamed: bool,
    /// 写入 $DSH_HOME 的文件数（home/ 或 dshhome home 级内容）
    pub home_written: usize,
    /// 覆盖前备份目录（home/.dshpm-import-bak-<ts>/）
    pub backup_dir: String,
    pub target_path: String,
}

/// manifest v5 解析（profile / dshhome 两形态）
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PackManifest {
    manifest_version: u32,
    #[serde(rename = "type", default)]
    pack_type: String,
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    display_name: Option<serde_json::Value>,
    #[serde(default)]
    profile_name: Option<String>,
    #[serde(default)]
    dsh_version: Option<String>,
    #[serde(default)]
    bundles: Vec<String>,
    #[serde(default)]
    dependencies: HashMap<String, String>,
    #[serde(default)]
    patch: Option<String>,
    #[serde(default)]
    default_profile: Option<String>,
    #[serde(default)]
    profiles: HashMap<String, ProfileUnit>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct ProfileUnit {
    #[serde(default)]
    bundles: Vec<String>,
    #[serde(default)]
    dependencies: HashMap<String, String>,
    #[serde(default)]
    patch: Option<String>,
}

// ==================== 安全过滤（DSH-PackForge 五类） ====================

fn is_excluded(rel: &str) -> bool {
    let rel = rel.trim_start_matches('/');
    let segs: Vec<&str> = rel.split('/').collect();
    let name = segs.last().unwrap_or(&"");
    // 1) 精确名（任意路径段命中）
    for seg in &segs {
        match *seg {
            "node_modules" | "dist" | "build" | "coverage" | ".cache" | "cordis.yml"
            | "manifest.json" | "package-lock.json" | "yarn.lock" | ".env" | ".npmrc"
            | ".credentials.yaml" | ".anonymous-user-id" | "settings.yaml" | ".dshpkcfg"
            | "data" | "sessions" | "logs" | ".git" | ".import-tmp" | ".pnpm" | ".dshpm-import-bak" => {
                return true;
            }
            _ => {}
        }
    }
    let lower = name.to_lowercase();
    // 2) 扩展名：密钥材料
    for ext in [".key", ".pem", ".p12", ".pfx", ".crt", ".der", ".asc"] {
        if lower.ends_with(ext) {
            return true;
        }
    }
    // 3) 文件名正则：凭据类
    if name.starts_with(".env")
        || name.starts_with("credentials")
        || name.ends_with(".credentials")
        || name.starts_with("id_rsa")
        || name.starts_with("secrets")
        || name.contains("token")
        || name.contains("api_key")
    {
        return true;
    }
    // 4) 相对路径正则：禁止嵌套压缩包
    if lower.ends_with(".tgz")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".zip")
        || lower.ends_with(".dspack")
    {
        return true;
    }
    // 5) 路径前缀：home 级运行时 / 基线目录
    if rel.starts_with("attachments/")
        || rel.starts_with("profiles/web/")
        || rel.starts_with("profiles/headless/")
        || rel.starts_with("skills/.system/")
    {
        return true;
    }
    false
}

/// 机器文件放包根，其余进 overrides/
fn is_machine_file(rel: &str) -> bool {
    matches!(rel, "package.json" | "pnpm-workspace.yaml" | "pnpm-lock.yaml")
}

/// 递归收集 (rel, abs)
fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("读取目录失败 {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        // 符号链接整体跳过（防逃逸/死循环）
        let ft = fs::symlink_metadata(&path)
            .map(|m| m.file_type())
            .map_err(|e| format!("metadata 失败 {}: {e}", path.display()))?;
        if ft.is_symlink() {
            continue;
        }
        if path.is_dir() {
            if !is_excluded(&rel) {
                collect_files(&path, &rel, out)?;
            }
        } else if !is_excluded(&rel) {
            out.push((rel, path));
        }
    }
    Ok(())
}

fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

/// 读 profile package.json：提取 bundles / dependencies
fn read_profile_pkg(profile_dir: &Path) -> (Vec<String>, HashMap<String, String>) {
    let pkg_path = profile_dir.join("package.json");
    let raw = fs::read_to_string(&pkg_path).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
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
    let deps = v
        .get("dependencies")
        .and_then(|d| d.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, val)| (k.clone(), val.as_str().unwrap_or("").to_string()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    (bundles, deps)
}

/// 解析 displayName / description（字符串或 map，zh-CN 优先）
fn display_str(v: Option<&serde_json::Value>, fallback: &str) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Object(m)) => {
            for key in ["zh-CN", "zh_CN", "zh"] {
                if let Some(serde_json::Value::String(s)) = m.get(key) {
                    return s.clone();
                }
            }
            m.values()
                .find_map(|x| x.as_str().map(String::from))
                .unwrap_or_else(|| fallback.to_string())
        }
        _ => fallback.to_string(),
    }
}

/// slug 化（小写，非 [a-z0-9-] 转 -）
fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "pack".to_string()
    } else {
        trimmed
    }
}

/// niceName：去 dsh-profile- / dsh- 前缀
fn nice_name(s: &str) -> String {
    s.trim_start_matches("dsh-profile-")
        .trim_start_matches("dsh-")
        .to_string()
}

// ==================== 导出整合包 ====================

/// 导出 profile 为 .dspack v3（profile 形态）
pub fn export_pack(
    profile_dir: &Path,
    profiles_dir: &Path,
    target: &Path,
    pack_name: &str,
    pack_version: &str,
    display_name: &str,
    dsh_version: &str,
) -> Result<PackExportResult, String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    if target.exists() {
        return Err(format!("输出文件已存在: {}", target.display()));
    }

    let profile_name = profile_dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let name = slugify(pack_name);
    let version = if pack_version.trim().is_empty() { "1.0.0" } else { pack_version.trim() };
    let display = if display_name.trim().is_empty() {
        nice_name(&profile_name)
    } else {
        display_name.trim().to_string()
    };

    // 1) 收集文件
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(profile_dir, "", &mut files)?;
    if files.is_empty() {
        return Err("未找到可打包的文件（profile 为空或全部被安全过滤）".to_string());
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    // 2) manifest
    let (bundles, deps) = read_profile_pkg(profile_dir);
    let patch = fs::read_to_string(profile_dir.join("cordis.patch.yml")).unwrap_or_default();
    let manifest = serde_json::json!({
        "manifestVersion": 5,
        "type": "profile",
        "name": name,
        "version": version,
        "displayName": display,
        "profileName": profile_name,
        "dshVersion": if dsh_version.trim().is_empty() { "" } else { dsh_version.trim() },
        "bundles": bundles,
        "dependencies": deps,
        "patch": patch,
    });

    // 3) 拼 ZIP
    let file = fs::File::create(target).map_err(|e| format!("创建 .dspack 失败: {e}"))?;
    let mut zip = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    let mut count = 0usize;
    // 机器文件 → 根；其余 → overrides/
    for (rel, abs) in &files {
        let data = fs::read(abs).map_err(|e| format!("读取文件失败 {}: {e}", abs.display()))?;
        let zip_name = if is_machine_file(rel) {
            rel.clone()
        } else {
            format!("overrides/{rel}")
        };
        zip.start_file(&zip_name, opts)
            .map_err(|e| format!("写包失败 {zip_name}: {e}"))?;
        zip.write_all(&data)
            .map_err(|e| format!("写包内容失败 {zip_name}: {e}"))?;
        count += 1;
    }
    // dspack.json + manifest.json 最后写入，覆盖任何扫描残留
    let dspack = serde_json::json!({ "format": "dspack", "version": 3 });
    for (zip_name, content) in [
        ("dspack.json", serde_json::to_vec(&dspack).unwrap()),
        ("manifest.json", serde_json::to_vec_pretty(&manifest).unwrap()),
    ] {
        zip.start_file(zip_name, opts).map_err(|e| format!("写包失败 {zip_name}: {e}"))?;
        zip.write_all(&content).map_err(|e| format!("写包内容失败 {zip_name}: {e}"))?;
        count += 1;
    }
    zip.finish().map_err(|e| format!("完成 .dspack 失败: {e}"))?;

    let zip_size = fs::metadata(target).map(|m| m.len()).unwrap_or(0);
    let raw = fs::read(target).map_err(|e| format!("读取 .dspack 失败: {e}"))?;
    Ok(PackExportResult {
        file_count: count,
        zip_size,
        sha256: sha256_hex(&raw),
        target_path: target.to_string_lossy().to_string(),
    })
}

// ==================== 市场 ====================

const MARKET_INDEX_URL: &str =
    "https://raw.githubusercontent.com/DSH-PackForge/dsh-pack-market/main/index/index.json";

/// 拉取整合包市场索引
pub fn read_market_index() -> Result<Vec<MarketPackEntry>, String> {
    let mirror = crate::settings::load_settings().github_mirror;
    let index_url = crate::settings::github_proxy(MARKET_INDEX_URL, &mirror);
    let resp = reqwest::blocking::Client::new()
        .get(&index_url)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .map_err(|e| format!("拉取整合包市场失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("市场索引返回状态 {}", resp.status()));
    }
    let idx: MarketIndex = resp.json().map_err(|e| format!("解析市场索引失败: {e}"))?;
    let mut out = Vec::new();
    for item in idx.modpacks {
        let e: serde_json::Value = item;
        let name = e.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        let display = display_str(e.get("displayName"), &name);
        let desc = display_str(e.get("description"), "");
        out.push(MarketPackEntry {
            name,
            version: e.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            display_name: display,
            description: desc,
            author: e.get("author").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            category: e.get("category").and_then(|x| x.as_str()).unwrap_or("uncategorized").to_string(),
            dsh_version: e.get("dshVersion").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            profile_name: e.get("profileName").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            download_url: e.get("downloadUrl").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            sha256: e.get("sha256").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            size: e.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
            updated_at: e.get("updatedAt").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            id: e.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            owner: e.get("owner").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            repo: e.get("repo").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            bundle_count: e.get("bundleCount").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            dep_count: e.get("depCount").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            profile_count: e.get("profileCount").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            manifest_version: e.get("manifestVersion").and_then(|x| x.as_u64()).unwrap_or(5) as u32,
            pack_type: e.get("type").and_then(|x| x.as_str()).unwrap_or("profile").to_string(),
        });
    }
    Ok(out)
}

/// 下载整合包到缓存目录，校验 sha256 + size
pub fn download_pack(
    url: &str,
    sha256: &str,
    size: u64,
    cache_dir: &Path,
) -> Result<String, String> {
    fs::create_dir_all(cache_dir).map_err(|e| format!("创建缓存目录失败: {e}"))?;
    let mirror = crate::settings::load_settings().github_mirror;
    let url = crate::settings::github_proxy(url, &mirror);
    let file_name = url
        .rsplit('/')
        .next()
        .filter(|s| s.ends_with(".dspack"))
        .unwrap_or("pack.dspack")
        .to_string();
    let target = cache_dir.join(&file_name);
    if target.exists() {
        fs::remove_file(&target).map_err(|e| format!("清理旧包失败: {e}"))?;
    }
    let resp = reqwest::blocking::Client::new()
        .get(url)
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .map_err(|e| format!("下载整合包失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载返回状态 {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .map_err(|e| format!("读取下载内容失败: {e}"))?;
    if size > 0 && bytes.len() as u64 != size {
        return Err(format!("包大小不符：预期 {size} 字节，实际 {} 字节", bytes.len()));
    }
    if !sha256.is_empty() && sha256_hex(&bytes) != sha256.to_lowercase() {
        return Err("包 SHA-256 校验失败：包不完整或已被篡改".to_string());
    }
    fs::write(&target, &bytes).map_err(|e| format!("写缓存失败: {e}"))?;
    Ok(target.to_string_lossy().to_string())
}

// ==================== 导入整合包 ====================

/// 解压 .dspack 到临时目录，校验 dspack.json + manifest
fn extract_and_parse(pack_path: &Path, tmp: &Path) -> Result<PackManifest, String> {
    let file = fs::File::open(pack_path).map_err(|e| format!("打开整合包失败: {e}"))?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("解析整合包失败: {e}"))?;
    fs::create_dir_all(tmp).map_err(|e| format!("创建临时目录失败: {e}"))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读取包条目失败: {e}"))?;
        let name = entry.name().to_string();
        let clean = Path::new(&name);
        if clean.is_absolute() || name.split('/').any(|seg| seg == "..") {
            return Err(format!("包包含非法路径: {name}"));
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

    // 校验 dspack.json
    let dspack_path = tmp.join("dspack.json");
    let dspack: serde_json::Value = fs::read_to_string(&dspack_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .ok_or("不是有效的 .dspack：缺少 dspack.json")?;
    if dspack.get("format").and_then(|x| x.as_str()) != Some("dspack") {
        return Err("不是有效的 .dspack：format 不是 dspack".to_string());
    }
    if dspack.get("version").and_then(|x| x.as_u64()) != Some(3) {
        return Err("不支持的 .dspack 容器版本（仅支持 v3）".to_string());
    }
    // 读 manifest
    let manifest_path = tmp.join("manifest.json");
    let raw = fs::read_to_string(&manifest_path)
        .map_err(|e| format!("读取 manifest.json 失败: {e}"))?;
    let m: PackManifest =
        serde_json::from_str(&raw).map_err(|e| format!("解析 manifest 失败: {e}"))?;
    if m.manifest_version != 5 {
        return Err(format!(
            "不支持的 manifest 版本 {}（仅支持 v5）",
            m.manifest_version
        ));
    }
    if m.pack_type != "profile" && m.pack_type != "dshhome" {
        return Err(format!("不支持的整合包类型: {}", m.pack_type));
    }
    if m.name.is_empty() {
        return Err("manifest 缺少 name".to_string());
    }
    Ok(m)
}

/// 复制目录树（src 下全部文件到 dst 的相对路径）
fn copy_tree(src: &Path, dst: &Path, prefix: &str) -> Result<usize, String> {
    let mut count = 0usize;
    let base = src.join(prefix);
    if !base.is_dir() {
        return Ok(0);
    }
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(&base, "", &mut files)?;
    // 包内内容不再二次过滤：解压时已校验路径，过滤只用于导出
    for (rel, abs) in files {
        let target = dst.join(&rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("创建目录失败 {}: {e}", parent.display()))?;
        }
        fs::copy(&abs, &target).map_err(|e| format!("复制文件失败 {}: {e}", abs.display()))?;
        count += 1;
    }
    Ok(count)
}

/// 覆盖写入目录树，已存在文件先备份到 backup_dir
fn copy_tree_with_backup(src: &Path, dst: &Path, backup: &Path) -> Result<usize, String> {
    let mut count = 0usize;
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(src, "", &mut files)?;
    for (rel, abs) in files {
        let target = dst.join(&rel);
        if target.exists() {
            // 备份已存在文件
            let bak = backup.join(&rel);
            if let Some(parent) = bak.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("创建备份目录失败: {e}"))?;
            }
            fs::copy(&target, &bak)
                .map_err(|e| format!("备份文件失败 {}: {e}", target.display()))?;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("创建目录失败 {}: {e}", parent.display()))?;
        }
        fs::copy(&abs, &target).map_err(|e| format!("复制文件失败 {}: {e}", abs.display()))?;
        count += 1;
    }
    Ok(count)
}

/// 重建 profile package.json（manifest 权威）
fn rebuild_package_json(
    profile_dir: &Path,
    final_name: &str,
    bundles: &[String],
    deps: &HashMap<String, String>,
) -> Result<(), String> {
    let pkg = serde_json::json!({
        "name": format!("dsh-profile-{final_name}"),
        "private": true,
        "dependencies": deps,
        "dsh": { "profile": { "bundles": bundles } }
    });
    fs::write(
        profile_dir.join("package.json"),
        serde_json::to_string_pretty(&pkg).unwrap(),
    )
    .map_err(|e| format!("写入 package.json 失败: {e}"))
}

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

/// 取不冲突的 profile 名
fn unique_name(base: &str, profiles_dir: &Path) -> (String, bool) {
    let base = sanitize_name(base);
    if !profiles_dir.join(&base).exists() {
        return (base, false);
    }
    let mut counter = 1;
    loop {
        let cand = format!("{base}-import-{counter}");
        if !profiles_dir.join(&cand).exists() {
            return (cand, true);
        }
        counter += 1;
        if counter > 100 {
            return (format!("{base}-import"), true);
        }
    }
}

/// 导入整合包到目标 profiles 目录；home 级内容写 profiles_dir 的父目录（DSH_HOME 语义）
pub fn import_pack(pack_path: &Path, profiles_dir: &Path) -> Result<PackImportResult, String> {
    let work = profiles_dir.join(".import-tmp");
    if work.exists() {
        fs::remove_dir_all(&work).map_err(|e| format!("清理临时目录失败: {e}"))?;
    }
    let manifest = extract_and_parse(pack_path, &work)?;
    let home_dir = profiles_dir.parent().unwrap_or(profiles_dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup_dir = home_dir.join(format!(".dshpm-import-bak-{ts}"));

    let mut final_names: Vec<String> = Vec::new();
    let mut renamed = false;
    let mut home_written = 0usize;
    let mut created_dirs: Vec<PathBuf> = Vec::new();
    let mut rollback_err: Option<String> = None;

    // 回滚闭包：删除本次新建的 profile 目录（home 覆盖已备份到 backup_dir）
    let rollback = |created: &[PathBuf]| {
        for d in created {
            let _ = fs::remove_dir_all(d);
        }
    };

    let result = (|| -> Result<(), String> {
        if manifest.pack_type == "profile" {
            let base = manifest
                .profile_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| manifest.name.clone());
            let (final_name, rn) = unique_name(&base, profiles_dir);
            renamed = rn;
            let dest = profiles_dir.join(&final_name);
            fs::create_dir_all(&dest).map_err(|e| format!("创建 profile 目录失败: {e}"))?;
            created_dirs.push(dest.clone());

            // overrides/ → profile 根
            let ov = work.join("overrides");
            if ov.is_dir() {
                copy_tree(&ov, &dest, "")?;
            }
            // manifest.patch（无文件时）
            if !dest.join("cordis.patch.yml").exists() {
                if let Some(patch) = manifest.patch.clone() {
                    if !patch.trim().is_empty() {
                        fs::write(dest.join("cordis.patch.yml"), patch)
                            .map_err(|e| format!("写入 patch 失败: {e}"))?;
                    }
                }
            }
            // 重建 package.json（manifest 权威）
            rebuild_package_json(&dest, &final_name, &manifest.bundles, &manifest.dependencies)?;
            // 根 lock/workspace 快照
            for f in ["pnpm-lock.yaml", "pnpm-workspace.yaml"] {
                let src = work.join(f);
                if src.is_file() {
                    fs::copy(&src, dest.join(f))
                        .map_err(|e| format!("复制 {f} 失败: {e}"))?;
                }
            }
            // home/ → $DSH_HOME（覆盖前备份）
            let home_src = work.join("home");
            if home_src.is_dir() {
                home_written = copy_tree_with_backup(&home_src, home_dir, &backup_dir)?;
            }
            final_names.push(final_name);
            Ok(())
        } else {
            // dshhome：逐 profile + home 级内容
            if manifest.profiles.is_empty() {
                return Err("dshhome 整合包不含任何 profile".to_string());
            }
            let mut keys: Vec<String> = manifest.profiles.keys().cloned().collect();
            keys.sort();
            let default_p = manifest.default_profile.clone().unwrap_or_default();
            // 优先把 defaultProfile 排第一（设为选中）
            if let Some(pos) = keys.iter().position(|k| *k == default_p) {
                keys.swap(0, pos);
            }
            for key in keys {
                if key == "web" || key == "headless" {
                    return Err(format!("dshhome 包含系统模板 profile: {key}"));
                }
                let unit = manifest
                    .profiles
                    .get(&key)
                    .cloned()
                    .unwrap_or_default();
                let (final_name, rn) = unique_name(&key, profiles_dir);
                if rn {
                    renamed = true;
                }
                let dest = profiles_dir.join(&final_name);
                fs::create_dir_all(&dest).map_err(|e| format!("创建 profile 目录失败: {e}"))?;
                created_dirs.push(dest.clone());
                let unit_dir = work.join("overrides").join("profiles").join(&key);
                if unit_dir.is_dir() {
                    copy_tree(&unit_dir, &dest, "")?;
                }
                if let Some(patch) = unit.patch.clone() {
                    if !dest.join("cordis.patch.yml").exists() && !patch.trim().is_empty() {
                        fs::write(dest.join("cordis.patch.yml"), patch)
                            .map_err(|e| format!("写入 patch 失败: {e}"))?;
                    }
                }
                rebuild_package_json(&dest, &final_name, &unit.bundles, &unit.dependencies)?;
                final_names.push(final_name);
            }
            // home 级其余内容：overrides 中非 profiles/ 的部分
            let ov = work.join("overrides");
            if ov.is_dir() {
                let mut files: Vec<(String, PathBuf)> = Vec::new();
                collect_files(&ov, "", &mut files)?;
                let filtered: Vec<(String, PathBuf)> = files
                    .into_iter()
                    .filter(|(rel, _)| !rel.starts_with("profiles/"))
                    .collect();
                if !filtered.is_empty() {
                    let staging = work.join(".home-staging");
                    fs::create_dir_all(&staging).map_err(|e| format!("创建暂存目录失败: {e}"))?;
                    for (rel, abs) in filtered {
                        let target = staging.join(&rel);
                        if let Some(parent) = target.parent() {
                            fs::create_dir_all(parent)
                                .map_err(|e| format!("创建目录失败: {e}"))?;
                        }
                        fs::copy(&abs, &target)
                            .map_err(|e| format!("复制文件失败 {}: {e}", abs.display()))?;
                    }
                    home_written = copy_tree_with_backup(&staging, home_dir, &backup_dir)?;
                }
            }
            Ok(())
        }
    })();

    if let Err(e) = result {
        rollback(&created_dirs);
        let _ = fs::remove_dir_all(&work);
        let _ = fs::remove_dir_all(&backup_dir);
        return Err(format!("导入失败（已回滚）：{e}"));
    }

    let target_path = final_names
        .first()
        .map(|n| profiles_dir.join(n).to_string_lossy().to_string())
        .unwrap_or_default();
    let _ = fs::remove_dir_all(&work);
    Ok(PackImportResult {
        pack_name: manifest.name,
        pack_version: manifest.version,
        pack_type: manifest.pack_type,
        final_names,
        renamed,
        home_written,
        backup_dir: backup_dir.to_string_lossy().to_string(),
        target_path,
    })
}

// ==================== 备注存储 ====================

pub fn notes_file() -> PathBuf {
    let dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.join("dsh-plugin-manager").join("profile-notes.json")
}

pub type NotesMap = HashMap<String, ProfileNote>;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProfileNote {
    pub note: String,
    pub hint_version: String,
}

pub fn load_notes() -> NotesMap {
    std::fs::read_to_string(notes_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_note(key: &str, note: &ProfileNote) -> Result<(), String> {
    let mut map = load_notes();
    if note.note.trim().is_empty() && note.hint_version.trim().is_empty() {
        map.remove(key);
    } else {
        map.insert(
            key.to_string(),
            ProfileNote {
                note: note.note.trim().to_string(),
                hint_version: note.hint_version.trim().to_string(),
            },
        );
    }
    let path = notes_file();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let raw = serde_json::to_string_pretty(&map).map_err(|e| format!("序列化失败: {e}"))?;
    fs::write(path, raw).map_err(|e| format!("写入备注失败: {e}"))
}

/// 备注 key：name::profilesDir（同名不同来源可独立备注）
pub fn note_key(name: &str, profiles_dir: &str) -> String {
    format!("{name}::{}", profiles_dir.trim_end_matches('/').trim_end_matches('\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dshpm-pf-{tag}-{}", std::process::id()));
        if d.exists() {
            let _ = fs::remove_dir_all(&d);
        }
        d
    }

    fn make_profile(dir: &Path, name: &str, bundles: &[&str]) {
        let pd = dir.join(name);
        fs::create_dir_all(pd.join("data")).unwrap();
        fs::create_dir_all(pd.join("node_modules").join("x")).unwrap();
        let pkg = serde_json::json!({
            "name": format!("dsh-profile-{name}"),
            "private": true,
            "dependencies": { "dsh-pet": "0.2.0" },
            "dsh": { "profile": { "bundles": bundles } }
        });
        fs::write(pd.join("package.json"), serde_json::to_string_pretty(&pkg).unwrap()).unwrap();
        fs::write(pd.join("cordis.patch.yml"), "patch: 1\n").unwrap();
        fs::write(pd.join("data").join("s.md"), "会话数据不入包").unwrap();
        fs::write(pd.join("node_modules").join("x").join("i.js"), "dep").unwrap();
        fs::write(pd.join(".credentials.yaml"), "SECRET").unwrap();
        fs::write(pd.join("secret.key"), "KEY").unwrap();
    }

    #[test]
    fn test_slugify_and_nice() {
        assert_eq!(slugify("My Pack!"), "my-pack");
        assert_eq!(slugify("web"), "web");
        assert_eq!(slugify("..."), "pack");
        assert_eq!(nice_name("dsh-profile-web"), "web");
        assert_eq!(nice_name("dsh-pet"), "pet");
    }

    #[test]
    fn test_exclusions() {
        assert!(is_excluded("node_modules/x/i.js"));
        assert!(is_excluded("data/s.md"));
        assert!(is_excluded("secret.key"));
        assert!(is_excluded(".credentials.yaml"));
        assert!(is_excluded("a/b/archive.zip"));
        assert!(is_excluded("a/b/c.tgz"));
        assert!(is_excluded("attachments/1.png"));
        assert!(!is_excluded("cordis.patch.yml"));
        assert!(!is_excluded("data2/keep.md"));
        assert!(!is_excluded("skills/my-skill/SKILL.md"));
    }

    #[test]
    fn test_export_pack_profile_form() {
        let work = tmpdir("exp");
        let profiles = work.join("profiles");
        make_profile(&profiles, "my-pack", &["@deepseek-ai/dsh-base"]);
        let out = work.join("my-pack-1.0.0.dspack");
        let r = export_pack(
            &profiles.join("my-pack"),
            &profiles,
            &out,
            "my-pack",
            "1.0.0",
            "我的整合包",
            "0.1.7-rc.2",
        )
        .unwrap();
        assert_eq!(r.file_count, 4); // package.json + cordis.patch.yml + dspack.json + manifest.json
        assert_eq!(r.zip_size, fs::metadata(&out).unwrap().len());
        assert_eq!(r.sha256.len(), 64);
        assert!(out.exists());

        // 结构校验
        let file = fs::File::open(&out).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.contains(&"dspack.json".to_string()));
        assert!(names.contains(&"manifest.json".to_string()));
        assert!(names.contains(&"package.json".to_string()));
        assert!(names.contains(&"overrides/cordis.patch.yml".to_string()));
        assert!(!names.iter().any(|n| n.contains("node_modules")), "node_modules 不应入包");
        assert!(!names.iter().any(|n| n.contains("data/")), "运行数据不应入包");
        assert!(!names.iter().any(|n| n.contains("credentials") || n.contains("secret")), "凭据不应入包");

        // dspack.json 内容
        let mut d = Vec::new();
        for i in 0..archive.len() {
            let mut e = archive.by_index(i).unwrap();
            if e.name() == "dspack.json" {
                std::io::copy(&mut e, &mut d).unwrap();
            }
        }
        let dj: serde_json::Value = serde_json::from_str(&String::from_utf8(d).unwrap()).unwrap();
        assert_eq!(dj["format"], "dspack");
        assert_eq!(dj["version"], 3);
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn test_import_pack_roundtrip_and_rename() {
        let work = tmpdir("imp");
        let src_profiles = work.join("src").join("profiles");
        make_profile(&src_profiles, "web", &["@deepseek-ai/dsh-base"]);
        let pack = work.join("web-1.0.0.dspack");
        export_pack(
            &src_profiles.join("web"),
            &src_profiles,
            &pack,
            "web",
            "1.0.0",
            "Web",
            "0.1.7-rc.2",
        )
        .unwrap();

        let dst = work.join("dst").join("profiles");
        fs::create_dir_all(&dst).unwrap();
        let r = import_pack(&pack, &dst).unwrap();
        assert_eq!(r.pack_type, "profile");
        assert_eq!(r.final_names, vec!["web".to_string()]);
        assert!(!r.renamed);
        let pdir = dst.join("web");
        assert!(pdir.join("package.json").exists());
        assert!(pdir.join("cordis.patch.yml").exists());
        let pkg: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(pdir.join("package.json")).unwrap()).unwrap();
        assert_eq!(pkg["name"], "dsh-profile-web");
        assert_eq!(pkg["dependencies"]["dsh-pet"], "0.2.0");
        assert_eq!(pkg["dsh"]["profile"]["bundles"][0], "@deepseek-ai/dsh-base");

        // 重名 → 改名导入
        let r2 = import_pack(&pack, &dst).unwrap();
        assert!(r2.renamed);
        assert!(r2.final_names[0].contains("import"));
        assert!(dst.join(&r2.final_names[0]).join("package.json").exists());

        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn test_import_rejects_non_dspack() {
        let work = tmpdir("bad");
        let dst = work.join("profiles");
        fs::create_dir_all(&dst).unwrap();
        let bad = work.join("bad.dspack");
        fs::write(&bad, "not a zip").unwrap();
        let r = import_pack(&bad, &dst);
        assert!(r.is_err());
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn test_notes_save_load() {
        // 用测试隔离：备份原文件
        let nf = notes_file();
        let orig = fs::read(&nf).ok();
        let key = "web::C:/x/profiles";
        let note = ProfileNote { note: " 适配旧版  ".into(), hint_version: " 0.1.0-rc.7 ".into() };
        save_note(key, &note).unwrap();
        let map = load_notes();
        assert_eq!(map.get(key).unwrap().note, "适配旧版");
        assert_eq!(map.get(key).unwrap().hint_version, "0.1.0-rc.7");
        // 清空 → 删除
        save_note(key, &ProfileNote::default()).unwrap();
        assert!(!load_notes().contains_key(key));
        // 还原
        match orig {
            Some(bytes) => fs::write(&nf, bytes).unwrap(),
            None => { let _ = fs::remove_file(&nf); }
        }
        let _ = nf;
    }
}
