//! DSH 多版本下载安装（npm --prefix 到独立目录）与 profile .npmrc 镜像配置

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use tauri::Emitter;

/// DSH 版本条目
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshVersionInfo {
    pub version: String,
    /// 是否为 dist-tag（latest/next/alpha）
    pub tags: Vec<String>,
}

/// 安装结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshInstallResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub summary: String,
    /// 安装后 dsh.cmd 路径
    pub bin_path: Option<String>,
    pub version: String,
}

/// 列出 @deepseek-ai/dsh 全部版本（dist-tag 标记）
pub fn list_dsh_versions(registry: &str) -> Result<Vec<DshVersionInfo>, String> {
    let info = crate::npm::npm_package_info("@deepseek-ai/dsh", registry)?;
    let tags: Vec<(String, String)> = info.dist_tags.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut out: Vec<DshVersionInfo> = info
        .versions
        .iter()
        .map(|v| {
            let mut tv = Vec::new();
            for (tag, ver) in &tags {
                if tag != "latest" && ver == &v.version {
                    tv.push(tag.clone());
                }
            }
            DshVersionInfo { version: v.version.clone(), tags: tv }
        })
        .collect();
    // 稳定排序：数值段降序（latest 优先放前面由 tags 标记体现）
    out.sort_by(|a, b| crate::npm::cmp_versions_pub(&b.version, &a.version));
    Ok(out)
}

/// 安装指定版本到 target_dir/dsh-<version>
pub fn install_dsh_version(
    app: &tauri::AppHandle,
    version: &str,
    target_dir: &str,
    registry: &str,
) -> DshInstallResult {
    let dir = Path::new(target_dir);
    let ver_dir = dir.join(format!("dsh-{version}"));
    if let Err(e) = std::fs::create_dir_all(&ver_dir) {
        return DshInstallResult { success: false, exit_code: None, summary: format!("创建目录失败: {e}"), bin_path: None, version: version.to_string() };
    }
    let bin = ver_dir.join("node_modules").join(".bin").join("dsh.cmd");
    if bin.exists() {
        return DshInstallResult {
            success: true,
            exit_code: Some(0),
            summary: format!("dsh {version} 已安装于 {}", ver_dir.display()),
            bin_path: Some(bin.to_string_lossy().to_string()),
            version: version.to_string(),
        };
    }
    let full = format!(
        "npm install \"@deepseek-ai/dsh@{}\" --prefix \"{}\" --registry \"{}\" --no-save --no-audit --no-fund",
        version,
        ver_dir.display(),
        registry
    );
    let _ = full;
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", &format!(
        "npm install \"@deepseek-ai/dsh@{}\" --prefix \"{}\" --registry \"{}\" --no-save --no-audit --no-fund",
        version, ver_dir.display(), registry
    )]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW：禁止弹出控制台黑框
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = app.emit("install-log", serde_json::json!({ "line": format!("启动 npm 失败: {e}"), "kind": "stderr" }));
            return DshInstallResult { success: false, exit_code: None, summary: format!("启动 npm 失败: {e}"), bin_path: None, version: version.to_string() };
        }
    };
    let out_handle = {
        let app = app.clone();
        let stdout = child.stdout.take();
        std::thread::spawn(move || {
            if let Some(stdout) = stdout {
                let reader = std::io::BufReader::new(stdout);
                for line in reader.lines() {
                    if let Ok(l) = line {
                        let _ = app.emit("install-log", serde_json::json!({ "line": l, "kind": "stdout" }));
                    }
                }
            }
        })
    };
    let err_handle = {
        let app = app.clone();
        let stderr = child.stderr.take();
        std::thread::spawn(move || {
            if let Some(stderr) = stderr {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    if let Ok(l) = line {
                        let _ = app.emit("install-log", serde_json::json!({ "line": l, "kind": "stderr" }));
                    }
                }
            }
        })
    };
    let status = child.wait();
    let _ = out_handle.join();
    let _ = err_handle.join();

    let (success, code) = match status {
        Ok(s) => (s.success(), s.code()),
        Err(_) => (false, None),
    };
    let _ = app.emit("install-done", serde_json::json!({ "success": success, "exitCode": code }));

    if !success {
        return DshInstallResult { success, exit_code: code, summary: format!("npm 安装失败（退出码 {code:?}）"), bin_path: None, version: version.to_string() };
    }
    // 探测 dsh.cmd
    let bin = ver_dir.join("node_modules").join(".bin").join("dsh.cmd");
    if !bin.exists() {
        return DshInstallResult { success: false, exit_code: code, summary: "安装完成但未找到 dsh.cmd".to_string(), bin_path: None, version: version.to_string() };
    }
    let mut probe = Command::new("cmd");
    probe.args(["/C", &format!("chcp 65001 >nul && \"{}\" --version", bin.display())]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        probe.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let probe_out = probe.output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let got_ver = if probe_out.is_empty() { version.to_string() } else { probe_out };
    DshInstallResult {
        success: true,
        exit_code: code,
        summary: format!("dsh {got_ver} 安装完成：{}", ver_dir.display()),
        bin_path: Some(bin.to_string_lossy().to_string()),
        version: got_ver,
    }
}

/// 确保 profile/.npmrc 含指定 registry（供 pnpm 插件安装走镜像）
pub fn ensure_profile_npmrc(profile_dir: &Path, registry: &str) -> Result<(), String> {
    let npmrc = profile_dir.join(".npmrc");
    let existing = std::fs::read_to_string(&npmrc).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().map(|l| l.to_string()).collect();
    let reg_line = format!("registry={registry}");
    let mut found = false;
    for l in &mut lines {
        if l.trim_start().starts_with("registry=") || l.trim_start().starts_with("registry =") {
            *l = reg_line.clone();
            found = true;
        }
    }
    if !found {
        lines.push(reg_line);
    }
    let content = lines.join("\n") + "\n";
    std::fs::write(&npmrc, content).map_err(|e| format!("写入 .npmrc 失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_profile_npmrc_new_and_replace() {
        let work = std::env::temp_dir().join(format!("dshpm-npmrc-{}", std::process::id()));
        if work.exists() {
            let _ = std::fs::remove_dir_all(&work);
        }
        std::fs::create_dir_all(&work).unwrap();
        // 新写
        ensure_profile_npmrc(&work, "https://a.com").unwrap();
        let c = std::fs::read_to_string(work.join(".npmrc")).unwrap();
        assert!(c.contains("registry=https://a.com"));
        // 替换
        ensure_profile_npmrc(&work, "https://b.com").unwrap();
        let c2 = std::fs::read_to_string(work.join(".npmrc")).unwrap();
        assert!(c2.contains("registry=https://b.com"));
        assert!(!c2.contains("https://a.com"));
        let _ = std::fs::remove_dir_all(&work);
    }

    #[test]
    fn test_list_dsh_versions_shape() {
        // 不依赖网络：直接构造
        let info = crate::npm::NpmPackageInfo {
            name: "@deepseek-ai/dsh".to_string(),
            description: String::new(),
            dist_tags: {
                let mut m = std::collections::HashMap::new();
                m.insert("latest".to_string(), "0.1.5-rc.3".to_string());
                m.insert("next".to_string(), "0.1.7-rc.2".to_string());
                m
            },
            versions: vec![
                crate::npm::NpmVersionInfo { version: "0.1.7-rc.2".to_string(), published_at: None, peer_dependencies: Default::default(), is_bundle: None },
                crate::npm::NpmVersionInfo { version: "0.1.5-rc.3".to_string(), published_at: None, peer_dependencies: Default::default(), is_bundle: None },
                crate::npm::NpmVersionInfo { version: "0.1.0-rc.7".to_string(), published_at: None, peer_dependencies: Default::default(), is_bundle: None },
            ],
        };
        // 复用 lib 逻辑做纯函数测试（不依赖命令）
        let mut out: Vec<DshVersionInfo> = info
            .versions
            .iter()
            .map(|v| {
                let mut tv = Vec::new();
                for (tag, ver) in &info.dist_tags {
                    if tag != "latest" && ver == &v.version {
                        tv.push(tag.clone());
                    }
                }
                DshVersionInfo { version: v.version.clone(), tags: tv }
            })
            .collect();
        out.sort_by(|a, b| crate::npm::cmp_versions_pub(&b.version, &a.version));
        assert_eq!(out[0].version, "0.1.7-rc.2");
        assert_eq!(out[0].tags, vec!["next"]);
        assert_eq!(out[1].version, "0.1.5-rc.3");
    }

    /// 真实网络验证：镜像源拉取 DSH 版本列表（默认忽略，手动执行
    /// `cargo test list_dsh_versions_live -- --ignored --nocapture`）
    #[test]
    #[ignore]
    fn list_dsh_versions_live() {
        let vs = crate::dsh_install::list_dsh_versions("https://registry.npmmirror.com").unwrap();
        assert!(!vs.is_empty(), "版本列表为空");
        println!("共 {} 个版本，前 3 个：", vs.len());
        for v in vs.iter().take(3) {
            println!("  {} tags={:?}", v.version, v.tags);
        }
    }
}
