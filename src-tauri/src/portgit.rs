//! 便携 Git（MinGit）检测与按需初始化（要求.md 二.6 / 二.7）
//!
//! 背景：pnpm 解析 `github:` / `git+` 等 **git 来源依赖**时必须有 git 命令。
//! 策略：
//! - 系统已有可用 Git（`git --version`）→ 直接用，不下载、不打扰
//! - 系统无 Git → 启动提示「初始化」；初始化 = 经 ghnet 镜像轮询下载 MinGit，
//!   解压到 `%AppData%\dsh-plugin-manager\runtime\git\`（**不内置进安装包**）
//! - 便携 Git 只注入**该子进程** PATH 前缀 `runtime\git\cmd`（供 pnpm 找到 git），
//!   **不修改系统/用户 PATH**，与 Node 便携运行时同一套约束

use std::path::{Path, PathBuf};
use std::process::Stdio;

/// API 拉不到最新版本时的兜底版本（git-for-windows 官方 release 长期存在）
const MINGIT_FALLBACK_VERSION: &str = "2.47.1";

/// 便携 Git 根目录：%AppData%\dsh-plugin-manager\runtime\git
pub fn runtime_git_dir() -> PathBuf {
    crate::toolchain::runtime_dir().join("git")
}

/// 便携 git.exe 路径（MinGit 布局：`cmd\git.exe`）；不存在 → None
pub fn portable_git_exe() -> Option<PathBuf> {
    let exe = runtime_git_dir().join("cmd").join("git.exe");
    exe.is_file().then_some(exe)
}

/// Git 来源
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitSource {
    /// 系统 PATH 里可用的 git
    System,
    /// runtime\git 内的便携 MinGit
    Portable,
    /// 系统、便携都没有
    None,
}

impl GitSource {
    /// 前端约定值："system" / "portable" / "none"
    pub fn as_str(&self) -> &'static str {
        match self {
            GitSource::System => "system",
            GitSource::Portable => "portable",
            GitSource::None => "none",
        }
    }
}

/// 系统 git 是否可用（`git --version` 探测，6s 超时；只探测，不改 PATH）
pub fn system_git_ok() -> bool {
    use std::os::windows::process::CommandExt;
    let mut child = match std::process::Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    // 超时轮询：git 挂起时 6s 后放弃，避免检测永久卡住
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => {
                let _ = child.kill();
                return false;
            }
        }
    }
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(_) => return false,
    };
    out.status.success()
        && String::from_utf8_lossy(&out.stdout)
            .to_lowercase()
            .contains("git version")
}

/// Git 来源：系统优先（要求：系统已有直接用），其次便携，都没有则 None
pub fn git_source() -> GitSource {
    if system_git_ok() {
        return GitSource::System;
    }
    if portable_git_exe().is_some() {
        return GitSource::Portable;
    }
    GitSource::None
}

/// git 是否可用（系统或便携任一）
pub fn git_available() -> bool {
    git_source() != GitSource::None
}

/// 便携 Git 注入子进程 PATH 的目录（仅便携存在时返回）：
/// `runtime\git\cmd`（要求二.6：PATH 前缀加 runtime\git\cmd，供 pnpm 找到 git）
pub fn path_dirs() -> Vec<String> {
    if portable_git_exe().is_none() {
        return Vec::new();
    }
    vec![runtime_git_dir().join("cmd").display().to_string()]
}

/// MinGit 下载直链（git-for-windows 官方 release）
pub fn mingit_download_url(ver: &str) -> String {
    format!(
        "https://github.com/git-for-windows/git/releases/download/v{ver}.windows.1/MinGit-{ver}-64-bit.zip"
    )
}

/// 从资产名解析 MinGit 版本号：`MinGit-2.47.1-64-bit.zip` → `2.47.1`
/// 只认 64-bit 正式包；32-bit / busybox 等变体一律拒绝
pub fn mingit_version_of_asset(name: &str) -> Option<String> {
    let rest = name.strip_prefix("MinGit-")?;
    let rest = rest.strip_suffix("-64-bit.zip")?;
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    if !rest.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(rest.to_string())
}

/// 从 GitHub API release JSON 里挑 MinGit-64-bit.zip → (版本, download_url)
pub fn pick_mingit_asset(release: &serde_json::Value) -> Option<(String, String)> {
    let assets = release.get("assets")?.as_array()?;
    for a in assets {
        let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if let Some(ver) = mingit_version_of_asset(name) {
            let url = a
                .get("browser_download_url")
                .and_then(|u| u.as_str())
                .unwrap_or("");
            let url = if url.is_empty() {
                mingit_download_url(&ver)
            } else {
                url.to_string()
            };
            return Some((ver, url));
        }
    }
    None
}

fn emit(app: &tauri::AppHandle, line: &str, kind: &str) {
    use tauri::Emitter;
    let _ = app.emit(
        "install-log",
        serde_json::json!({ "line": line, "kind": kind }),
    );
}

/// 解析 MinGit 下载目标：ghnet 镜像轮询拉 release 元数据取最新 64-bit 资产；
/// 失败时回退内置版本直链（同样走 ghnet）
fn resolve_mingit_target(app: &tauri::AppHandle) -> (String, String) {
    let api = "https://api.github.com/repos/git-for-windows/git/releases/latest";
    match crate::ghnet::fetch_text(api) {
        Ok((_, body)) => match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(json) => {
                if let Some(t) = pick_mingit_asset(&json) {
                    return t;
                }
                emit(
                    app,
                    "API 响应中未找到 MinGit-64-bit.zip，改用内置版本",
                    "stderr",
                );
            }
            Err(e) => emit(
                app,
                &format!("解析 MinGit 版本信息失败（{e}），改用内置版本"),
                "stderr",
            ),
        },
        Err(e) => emit(
            app,
            &format!("获取 MinGit 版本失败（{e}），改用内置版本"),
            "stderr",
        ),
    }
    let ver = MINGIT_FALLBACK_VERSION.to_string();
    let url = mingit_download_url(&ver);
    (ver, url)
}

/// 确保 git 可用：
/// - 系统已有 → `Ok(None)`（直接用系统 Git，不下载）
/// - 便携已有 → `Ok(Some(exe))`
/// - 都没有 → 下载解压 MinGit → `Ok(Some(exe))`
/// 失败返回中文报错（可换镜像、可再次初始化）
pub fn ensure_git(app: &tauri::AppHandle) -> Result<Option<PathBuf>, String> {
    if system_git_ok() {
        return Ok(None);
    }
    if let Some(p) = portable_git_exe() {
        return Ok(Some(p));
    }
    download_mingit(app).map(Some)
}

/// 下载 MinGit 并解压到 `runtime\git\`（幂等：`cmd\git.exe` 已存在则跳过）
pub fn download_mingit(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    if let Some(p) = portable_git_exe() {
        return Ok(p);
    }
    let dest = runtime_git_dir();
    let (ver, url) = resolve_mingit_target(app);
    emit(app, &format!("正在下载便携 Git（MinGit {ver}）…"), "stdout");
    emit(app, &url, "stdout");

    let (_used, resp) = crate::ghnet::send_get(&url).map_err(|e| {
        format!("便携 Git 下载失败：{e}（可更换 GitHub 镜像后再次初始化）")
    })?;
    std::fs::create_dir_all(&dest).map_err(|e| format!("创建 runtime\\git 失败: {e}"))?;
    let tmp = crate::toolchain::runtime_dir().join("git-mingit.zip");
    {
        let mut file = std::fs::File::create(&tmp).map_err(|e| format!("写入临时文件失败: {e}"))?;
        let mut resp = resp;
        std::io::copy(&mut resp, &mut file).map_err(|e| format!("保存下载内容失败: {e}"))?;
    }

    emit(app, "解压便携 Git…", "stdout");
    let extract_result = extract_mingit(&tmp, &dest);
    let _ = std::fs::remove_file(&tmp);
    extract_result?;

    let exe = dest.join("cmd").join("git.exe");
    if !exe.is_file() {
        return Err("便携 Git 解压完成但未找到 cmd\\git.exe，请再次初始化".to_string());
    }
    emit(
        app,
        &format!("便携 Git 安装完成：{}", exe.display()),
        "stdout",
    );
    Ok(exe)
}

/// 解压 MinGit zip 到 dest（兼容根级布局与单一顶层包裹目录两种打包方式）
fn extract_mingit(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let file = std::fs::File::open(zip_path).map_err(|e| format!("打开压缩包失败: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("解析 MinGit 压缩包失败: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("读取压缩条目失败: {e}"))?;
        let name = entry.name().replace('\\', "/");
        // 跳过绝对路径/穿越
        if name.contains("..") {
            continue;
        }
        let out = dest.join(&name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).ok();
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut f = std::fs::File::create(&out).map_err(|e| format!("写入 {name} 失败: {e}"))?;
        std::io::copy(&mut entry, &mut f).map_err(|e| format!("解压 {name} 失败: {e}"))?;
    }
    hoist_wrapper_dir(dest);
    Ok(())
}

/// 顶层包裹目录（如 `MinGit-2.47.1-64-bit\cmd\git.exe`）时，把内容上提一级；
/// 根级已是 `cmd\git.exe` 时不动。
fn hoist_wrapper_dir(dest: &Path) {
    if dest.join("cmd").join("git.exe").is_file() {
        return;
    }
    let mut wrapper: Option<PathBuf> = None;
    if let Ok(rd) = std::fs::read_dir(dest) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && p.join("cmd").join("git.exe").is_file() {
                wrapper = Some(p);
                break;
            }
        }
    }
    if let Some(w) = wrapper {
        if let Ok(rd) = std::fs::read_dir(&w) {
            for e in rd.flatten() {
                let _ = std::fs::rename(e.path(), dest.join(e.file_name()));
            }
        }
        let _ = std::fs::remove_dir_all(&w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mingit_version_of_asset_parses_official_name() {
        assert_eq!(
            mingit_version_of_asset("MinGit-2.47.1-64-bit.zip"),
            Some("2.47.1".to_string())
        );
        assert_eq!(
            mingit_version_of_asset("MinGit-2.45.2-64-bit.zip"),
            Some("2.45.2".to_string())
        );
    }

    #[test]
    fn mingit_version_of_asset_rejects_variants() {
        // 32-bit / busybox / PortableGit / 非 MinGit 一律不认
        assert_eq!(mingit_version_of_asset("MinGit-2.47.1-32-bit.zip"), None);
        assert_eq!(
            mingit_version_of_asset("MinGit-2.47.1-busybox-64-bit.zip"),
            None
        );
        assert_eq!(
            mingit_version_of_asset("PortableGit-2.47.1-64-bit.7z.exe"),
            None
        );
        assert_eq!(mingit_version_of_asset("Git-2.47.1-64-bit.exe"), None);
        assert_eq!(mingit_version_of_asset("MinGit--64-bit.zip"), None);
        assert_eq!(mingit_version_of_asset("MinGit-abc-64-bit.zip"), None);
    }

    #[test]
    fn mingit_download_url_uses_official_release() {
        assert_eq!(
            mingit_download_url("2.47.1"),
            "https://github.com/git-for-windows/git/releases/download/v2.47.1.windows.1/MinGit-2.47.1-64-bit.zip"
        );
    }

    #[test]
    fn pick_mingit_asset_prefers_64bit_official() {
        let json = serde_json::json!({
            "tag_name": "v2.47.1.windows.1",
            "assets": [
                {"name": "Git-2.47.1-64-bit.exe", "browser_download_url": "https://github.com/x/Git-2.47.1-64-bit.exe"},
                {"name": "MinGit-2.47.1-busybox-64-bit.zip", "browser_download_url": "https://github.com/x/busybox.zip"},
                {"name": "MinGit-2.47.1-64-bit.zip", "browser_download_url": "https://github.com/x/MinGit-2.47.1-64-bit.zip"},
                {"name": "MinGit-2.47.1-32-bit.zip", "browser_download_url": "https://github.com/x/m32.zip"},
            ]
        });
        let (ver, url) = pick_mingit_asset(&json).expect("应选中 64-bit MinGit");
        assert_eq!(ver, "2.47.1");
        assert_eq!(url, "https://github.com/x/MinGit-2.47.1-64-bit.zip");
    }

    #[test]
    fn pick_mingit_asset_none_when_absent() {
        let json = serde_json::json!({
            "assets": [
                {"name": "Git-2.47.1-64-bit.exe", "browser_download_url": "https://github.com/x/Git.exe"},
            ]
        });
        assert_eq!(pick_mingit_asset(&json), None);
        assert_eq!(pick_mingit_asset(&serde_json::json!({})), None);
    }

    #[test]
    fn pick_mingit_asset_falls_back_to_constructed_url() {
        let json = serde_json::json!({
            "assets": [
                {"name": "MinGit-2.46.0-64-bit.zip"}
            ]
        });
        let (ver, url) = pick_mingit_asset(&json).unwrap();
        assert_eq!(ver, "2.46.0");
        assert_eq!(url, mingit_download_url("2.46.0"));
    }

    #[test]
    fn path_dirs_empty_without_portable_git() {
        // 与便携可执行文件的存在性保持一致；不依赖本机是否装了便携 Git
        assert_eq!(path_dirs().is_empty(), portable_git_exe().is_none());
    }

    #[test]
    fn git_source_is_consistent_with_probes() {
        let s = git_source();
        // 三种来源之一，且与系统/便携探针一致
        assert!(matches!(s, GitSource::System | GitSource::Portable | GitSource::None));
        if system_git_ok() {
            assert_eq!(s, GitSource::System);
        } else if portable_git_exe().is_some() {
            assert_eq!(s, GitSource::Portable);
        } else {
            assert_eq!(s, GitSource::None);
        }
        assert_eq!(git_available(), s != GitSource::None);
    }

    #[test]
    fn hoist_wrapper_dir_lifts_single_wrapper() {
        let base = std::env::temp_dir().join(format!("dshpm-hoist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dest = base.join("git");
        let wrapper = dest.join("MinGit-2.47.1-64-bit");
        std::fs::create_dir_all(wrapper.join("cmd")).unwrap();
        std::fs::write(wrapper.join("cmd").join("git.exe"), b"x").unwrap();
        std::fs::write(wrapper.join("README.portable"), b"x").unwrap();
        hoist_wrapper_dir(&dest);
        assert!(dest.join("cmd").join("git.exe").is_file(), "包裹目录应被上提");
        assert!(dest.join("README.portable").is_file());
        assert!(!wrapper.exists(), "包裹目录应被清理");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn hoist_wrapper_dir_keeps_root_layout() {
        let base = std::env::temp_dir().join(format!("dshpm-hoist-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dest = base.join("git");
        std::fs::create_dir_all(dest.join("cmd")).unwrap();
        std::fs::write(dest.join("cmd").join("git.exe"), b"x").unwrap();
        hoist_wrapper_dir(&dest);
        assert!(dest.join("cmd").join("git.exe").is_file(), "根级布局应保持不动");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 真实网络：ghnet 拉 MinGit zip → 解压到临时目录 → `git --version` 可执行。
    /// 手动执行：`cargo test --lib real_download_mingit -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_download_mingit() {
        // 1) 解析下载目标（API 优先，失败回退内置版本；均走 ghnet 镜像轮询）
        let (ver, url) = resolve_mingit_target_for_test();
        eprintln!("MinGit {ver} ← {url}");
        let (_used, bytes) = crate::ghnet::fetch_bytes(&url).expect("MinGit 下载成功");
        assert!(bytes.len() > 10_000_000, "MinGit zip 应有几十 MB，实际 {}", bytes.len());

        // 2) 解压到临时目录并验证布局与可执行
        let base = std::env::temp_dir().join(format!("dshpm-mingit-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dest = base.join("git");
        std::fs::create_dir_all(&dest).unwrap();
        let tmp = base.join("mingit.zip");
        std::fs::write(&tmp, &bytes).unwrap();
        extract_mingit(&tmp, &dest).expect("解压成功");
        let exe = dest.join("cmd").join("git.exe");
        assert!(exe.is_file(), "解压后应有 cmd\\git.exe");

        // 3) MinGit 真的能跑（便携、不依赖安装）
        let out = crate::fsutil::hidden_command(&exe)
            .arg("--version")
            .output()
            .expect("运行 git --version");
        let s = String::from_utf8_lossy(&out.stdout);
        eprintln!("git --version → {s}");
        assert!(out.status.success() && s.to_lowercase().contains("git version"), "{s}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 真实测试用：不依赖 AppHandle 的目标解析（与 resolve_mingit_target 同逻辑）
    fn resolve_mingit_target_for_test() -> (String, String) {
        let api = "https://api.github.com/repos/git-for-windows/git/releases/latest";
        if let Ok((_, body)) = crate::ghnet::fetch_text(api) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(t) = pick_mingit_asset(&json) {
                    return t;
                }
            }
        }
        let ver = MINGIT_FALLBACK_VERSION.to_string();
        (ver.clone(), mingit_download_url(&ver))
    }
}
