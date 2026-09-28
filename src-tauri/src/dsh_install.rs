//! DSH 多版本下载安装（npm --prefix 到独立目录）与 profile .npmrc 镜像配置

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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

/// 构造 npm install 参数（在版本目录内安装，不走 --prefix，避免 cmd /C 嵌套引号问题）
/// cache_dir 用 DSH 下载根下的独立缓存：npm 下载的 tarball 先进缓存，进度监控统计它 +
/// node_modules 后，下载阶段就有可见进度（慢网下不再出现前 1-2 分钟毫无动静）。
pub fn build_npm_install_args(version: &str, registry: &str, cache_dir: &str) -> Vec<String> {
    let mut args = vec![
        "install".to_string(),
        format!("@deepseek-ai/dsh@{version}"),
        "--cache".to_string(),
        cache_dir.to_string(),
    ];
    let reg = registry.trim();
    if !reg.is_empty() {
        args.push("--registry".to_string());
        args.push(crate::settings::resolve_registry(reg));
    }
    args.extend(["--no-save".to_string(), "--no-audit".to_string(), "--no-fund".to_string()]);
    args
}

/// 安装指定版本到 target_dir/dsh-<version>
pub fn install_dsh_version(
    app: &tauri::AppHandle,
    version: &str,
    target_dir: &str,
    registry: &str,
) -> DshInstallResult {
    // 1) 下载前确保 node/npm/pnpm 环境（缺失自动安装）
    let _ = app.emit(
        "install-log",
        serde_json::json!({ "line": "正在检查工具链（node/npm/pnpm；缺失时自动下载安装）…", "kind": "stdout" }),
    );
    let tc = match crate::toolchain::ensure(app, registry) {
        Ok(t) => t,
        Err(e) => {
            let _ = app.emit("install-log", serde_json::json!({ "line": format!("工具链检查失败: {e}"), "kind": "stderr" }));
            return DshInstallResult { success: false, exit_code: None, summary: format!("工具链检查失败: {e}"), bin_path: None, version: version.to_string() };
        }
    };

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
    let cache_dir = dir.join(".npm-cache");
    let _ = std::fs::create_dir_all(&cache_dir);
    let args = build_npm_install_args(version, registry, &cache_dir.to_string_lossy());
    let _ = app.emit(
        "install-log",
        serde_json::json!({ "line": "正在解析依赖并下载安装（慢网首次可能需要 1-2 分钟无输出，属正常；下方进度会持续变化）…", "kind": "stdout" }),
    );
    let mut cmd = Command::new(&tc.npm);
    cmd.args(&args);
    cmd.current_dir(&ver_dir);
    tc.inject_path(&mut cmd);
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
    // ===== 安装进度监控 =====
    // npm 在非 TTY 下不输出进度条，前端只有日志。这里后台轮询 node_modules 体积增长
    // （npm install 期间逐步解压依赖），并提前查 npm dist.unpackedSize 作为参考总量，
    // 推算出可感的百分比进度。unpacked 结果放共享 AtomicU64，随时可取、不阻塞日志流。
    // 学习式总量：上次实际安装体积 > 包本体参考（×1.3，下限 50MB）
    let known_size = crate::settings::load_settings().dsh_install_sizes.get(version).copied().unwrap_or(0);
    let unpacked_arc = Arc::new(std::sync::atomic::AtomicU64::new(0));
    {
        let npm_probe = tc.npm.clone();
        let ver_arg = format!("@deepseek-ai/dsh@{version}");
        let reg = registry.to_string();
        let target = unpacked_arc.clone();
        std::thread::spawn(move || {
            let mut cmd = Command::new(&npm_probe);
            cmd.args(["view", &ver_arg, "dist.unpackedSize", "--json", "--no-audit", "--no-fund"]);
            if !reg.trim().is_empty() {
                cmd.arg("--registry").arg(crate::settings::resolve_registry(&reg));
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000);
            }
            cmd.stdout(Stdio::null()).stderr(Stdio::null()).stdin(Stdio::null());
            let n = cmd
                .output()
                .ok()
                .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().ok())
                .unwrap_or(0);
            target.store(n, Ordering::Relaxed);
        });
    }
    let stop_flag = Arc::new(AtomicBool::new(false));
    let monitor = {
        let app = app.clone();
        let ver_dir2 = ver_dir.clone();
        let cache2 = cache_dir.clone();
        let stop = stop_flag.clone();
        let unpacked_ref = unpacked_arc.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(500));
                // 下载的 tarball 先进缓存、解压进 node_modules：两者之和即“已处理数据量”
                let bytes = dir_size(&ver_dir2) + dir_size(&cache2);
                let unpacked = unpacked_ref.load(Ordering::Relaxed);
                // 总量优先级：学习到的实际体积 > 包本体×1.3（下限 50MB）> 0（未知）
                let cap = if known_size > 0 {
                    known_size
                } else if unpacked > 0 {
                    ((unpacked as f64 * 1.3) as u64).max(50 * 1024 * 1024)
                } else {
                    0
                };
                let percent = if cap > 0 {
                    (((bytes as f64 / cap as f64) * 100.0).min(95.0)) as u32
                } else {
                    u32::MAX // 未知总量：前端只显示 MB
                };
                let _ = app.emit(
                    "install-progress",
                    serde_json::json!({ "bytes": bytes, "total": cap, "percent": percent }),
                );
            }
        })
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
    stop_flag.store(true, Ordering::Relaxed);
    let _ = monitor.join();
    let _ = out_handle.join();
    let _ = err_handle.join();

    let (success, code) = match status {
        Ok(s) => (s.success(), s.code()),
        Err(_) => (false, None),
    };
    // 学习式进度：记录本次实际安装体积，下次安装同一版本即有准确百分比
    if success {
        let mut st = crate::settings::load_settings();
        st.dsh_install_sizes.insert(version.to_string(), dir_size(&ver_dir));
        let _ = crate::settings::save_settings(&st);
    }
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
    let _ = app.emit(
        "install-log",
        serde_json::json!({ "line": format!("✅ dsh {got_ver} 安装完成：{}", ver_dir.display()), "kind": "stdout" }),
    );
    DshInstallResult {
        success: true,
        exit_code: code,
        summary: format!("dsh {got_ver} 安装完成：{}", ver_dir.display()),
        bin_path: Some(bin.to_string_lossy().to_string()),
        version: got_ver,
    }
}

/// 递归统计目录总字节数（进度监控用；只统计常规文件，符号链接跳过避免环）
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(path) {
        for e in rd.flatten() {
            let p = e.path();
            if let Ok(ft) = e.file_type() {
                if ft.is_dir() {
                    total += dir_size(&p);
                } else if ft.is_file() {
                    if let Ok(m) = e.metadata() {
                        total += m.len();
                    }
                }
            }
        }
    }
    total
}

/// 确保 profile/.npmrc 含指定 registry（供 pnpm 插件安装走镜像）
pub fn ensure_profile_npmrc(profile_dir: &Path, registry: &str) -> Result<(), String> {
    let npmrc = profile_dir.join(".npmrc");
    let existing = std::fs::read_to_string(&npmrc).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().map(|l| l.to_string()).collect();
    let reg = registry.trim();
    if reg.is_empty() {
        // 空 = 不使用镜像源：移除 .npmrc 中所有 registry 行
        lines.retain(|l| {
            !(l.trim_start().starts_with("registry=") || l.trim_start().starts_with("registry ="))
        });
    } else {
        let reg_line = format!("registry={}", crate::settings::resolve_registry(reg));
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
    }
    let content = lines.join("\n") + "\n";
    std::fs::write(&npmrc, content).map_err(|e| format!("写入 .npmrc 失败: {e}"))
}

/// 在指定目录运行 `pnpm install`（走该目录 .npmrc 的镜像源），
/// stdout/stderr 逐行 emit "install-log"，返回退出码。
pub fn run_pnpm_install(app: &tauri::AppHandle, cwd: &std::path::Path) -> Result<i32, String> {
    let tc = crate::toolchain::probe();
    let pnpm = tc.pnpm.as_ref().ok_or_else(|| "未找到 pnpm（工具链会自动安装，请稍后重试）".to_string())?;
    let mut cmd = std::process::Command::new(pnpm);
    cmd.arg("install").arg("--no-frozen-lockfile").current_dir(cwd);
    tc.inject_path(&mut cmd);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW：禁止弹出控制台黑框
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());

    let mut child = cmd.spawn().map_err(|e| format!("启动 pnpm 失败: {e}"))?;
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
    match status {
        Ok(s) => Ok(s.code().unwrap_or(-1)),
        Err(e) => Err(format!("等待 pnpm 退出失败: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_size_counts_nested_bytes() {
        let tmp = std::env::temp_dir().join(format!("dshpm-dirsize-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("a/b")).unwrap();
        std::fs::write(tmp.join("a/f1"), vec![1u8; 100]).unwrap();
        std::fs::write(tmp.join("a/b/f2"), vec![2u8; 50]).unwrap();
        assert_eq!(dir_size(&tmp), 150);
        std::fs::remove_dir_all(&tmp).ok();
    }

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
        // 空 registry：移除已有行
        ensure_profile_npmrc(&work, "").unwrap();
        let c3 = std::fs::read_to_string(work.join(".npmrc")).unwrap();
        assert!(!c3.contains("registry="));
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
        assert!(!vs.is_empty());
        let tmpc = std::env::temp_dir().join("dshpm-npmcache-test");
        let args = crate::dsh_install::build_npm_install_args("0.1.7-rc.2", "https://registry.npmmirror.com", &tmpc.to_string_lossy());
        assert!(args.iter().any(|a| a == "--cache"));
        assert_eq!(args[0], "install");
        assert_eq!(args[1], "@deepseek-ai/dsh@0.1.7-rc.2");
        assert_eq!(args[3], "--registry");
        assert_eq!(args[4], "https://registry.npmmirror.com");
        assert!(args.contains(&"--no-audit".to_string()));
        assert!(!vs.is_empty(), "版本列表为空");
        println!("共 {} 个版本，前 3 个：", vs.len());
        for v in vs.iter().take(3) {
            println!("  {} tags={:?}", v.version, v.tags);
        }
    }
}
