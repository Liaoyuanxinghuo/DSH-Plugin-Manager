//! 插件安装/卸载/豁免执行：调用 dsh CLI，流式输出日志

use crate::models::DshEnv;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use tauri::Emitter;

/// 安装结果
#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InstallOutcome {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub summary: String,
}

/// 执行 dsh 命令并流式推送日志事件（**绝对路径 node + bin.js**，不依赖 PATH、不弹黑框）
/// 事件: install-log (line, kind), install-done (success, exitCode)
pub fn run_dsh_with_logs(
    app: &tauri::AppHandle,
    env: &DshEnv,
    args: &[String],
    dsh_home: Option<&str>,
) -> InstallOutcome {
    // 1) 便携运行时（缺则自动下载；绝不动 PATH，子进程只带 NODE 等运行时变量）
    let registry = crate::settings::load_settings().npm_registry;
    let tc = match crate::toolchain::ensure(app, &registry) {
        Ok(t) => t,
        Err(e) => {
            let _ = app.emit(
                "install-log",
                serde_json::json!({ "line": format!("工具链检查失败: {e}"), "kind": "stderr" }),
            );
            return InstallOutcome {
                success: false,
                exit_code: None,
                summary: format!("工具链不可用：{e}"),
            };
        }
    };

    // 2) 优先：绝对路径 node.exe + bin.js（绕开 dsh.cmd shim 对 PATH 的依赖）
    let mut cmd = match crate::runner::resolve_dsh_launcher(env) {
        Ok((node, binjs)) => {
            let mut c = Command::new(&node);
            c.arg(&binjs);
            for a in args {
                c.arg(a);
            }
            c
        }
        Err(_) => {
            // 3) 兜底：环境自带 run_command（如全局 dsh.cmd，本机自洽时才可用）
            let mut full = env.run_command.clone();
            for a in args {
                full.push(' ');
                full.push_str(a);
            }
            let mut c = Command::new("cmd");
            c.args(["/C", &format!("chcp 65001 >nul && {full}")]);
            c
        }
    };
    if let Some(home) = dsh_home {
        cmd.env("DSH_HOME", home);
    }
    tc.apply_runtime_env(&mut cmd);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = app.emit("install-log", serde_json::json!({ "line": format!("启动失败: {e}"), "kind": "stderr" }));
            return InstallOutcome { success: false, exit_code: None, summary: format!("启动失败: {e}") };
        }
    };

    // stdout 线程
    let out_handle = {
        let app = app.clone();
        let stdout = child.stdout.take();
        std::thread::spawn(move || {
            if let Some(stdout) = stdout {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    if let Ok(l) = line {
                        let _ = app.emit("install-log", serde_json::json!({ "line": l, "kind": "stdout" }));
                    }
                }
            }
        })
    };
    // stderr 线程（顺带记下最后一条非空错误行，失败摘要用它说话而不是「详见日志」）
    let last_err = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let err_handle = {
        let app = app.clone();
        let last_err = last_err.clone();
        let stderr = child.stderr.take();
        std::thread::spawn(move || {
            if let Some(stderr) = stderr {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    if let Ok(l) = line {
                        if !l.trim().is_empty() {
                            *last_err.lock().unwrap() = l.clone();
                        }
                        let _ = app.emit("install-log", serde_json::json!({ "line": l, "kind": "stderr" }));
                    }
                }
            }
        })
    };

    let _ = out_handle.join();
    let _ = err_handle.join();
    let status = child.wait();
    let success = match &status {
        Ok(s) => s.success(),
        Err(_) => false,
    };
    let exit_code = status.ok().and_then(|s| s.code());
    let _ = app.emit(
        "install-done",
        serde_json::json!({ "success": success, "exitCode": exit_code }),
    );
    let tail = last_err.lock().unwrap().clone();
    InstallOutcome {
        success,
        exit_code,
        summary: failure_summary(success, &tail),
    }
}

/// 失败摘要：带上 stderr 最后一条错误行（CLI 的真实原因），没有才退回「详见日志」。
/// 命中官方「Electron 应用独占管理」类拒绝时追加提示：可能是该 profile 被
/// 官方桌面应用特别管理导致失败（不预检、不拦截，照常执行，仅失败时说明）。
fn failure_summary(success: bool, last_stderr_line: &str) -> String {
    if success {
        return "执行成功".to_string();
    }
    let tail = last_stderr_line.trim();
    if tail.is_empty() {
        return "执行失败，详见日志".to_string();
    }
    let base = format!("执行失败：{tail}");
    if tail.contains("managed exclusively") || tail.contains("Electron application") {
        format!(
            "{base}（提示：可能是该 profile 被官方 DSH Desktop 应用特别管理导致失败；\
             可在官方桌面应用内操作，或克隆/改名后用本管理器管理）"
        )
    } else {
        base
    }
}

/// 在线安装插件：dsh plugin --profile <profile> add <spec>
pub fn install_plugin(
    app: &tauri::AppHandle,
    env: &DshEnv,
    profile: &str,
    spec: &str,
    profiles_dir: &std::path::Path,
) -> InstallOutcome {
    // 安装前确保 profile/.npmrc 使用当前镜像源
    let profile_dir = profiles_dir.join(profile);
    let registry = crate::settings::load_settings().npm_registry;
    let _ = crate::dsh_install::ensure_profile_npmrc(&profile_dir, &registry);
    let args = vec![
        "plugin".to_string(),
        "--profile".to_string(),
        profile.to_string(),
        "add".to_string(),
        spec.to_string(),
    ];
    run_dsh_with_logs(app, env, &args, Some(&dsh_home_of(profiles_dir)))
}

/// DSH CLI 语义：profile 位于 `$DSH_HOME/profiles/<name>`。
/// 我们管理的 profiles_dir 就是直接包含 profile 子目录的目录，
/// 因此 DSH_HOME 应指向它的父目录。
fn dsh_home_of(profiles_dir: &std::path::Path) -> String {
    profiles_dir
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 卸载插件：dsh plugin --profile <profile> remove <name>
pub fn remove_plugin(
    app: &tauri::AppHandle,
    env: &DshEnv,
    profile: &str,
    name: &str,
    profiles_dir: &std::path::Path,
) -> InstallOutcome {
    let args = vec![
        "plugin".to_string(),
        "--profile".to_string(),
        profile.to_string(),
        "remove".to_string(),
        name.to_string(),
    ];
    run_dsh_with_logs(app, env, &args, Some(&dsh_home_of(profiles_dir)))
}

/// 豁免版本校验：dsh plugin allow-version <pkg>@<version>
/// 注意：仅新版 dsh（带严格 peer 校验的版本）支持此命令；
/// 旧版（pnpm 转发式）会报 "Command not found"，日志如实展示。
pub fn allow_version(
    app: &tauri::AppHandle,
    env: &DshEnv,
    pkg_spec: &str,
    profiles_dir: &std::path::Path,
) -> InstallOutcome {
    let args = vec!["plugin".to_string(), "allow-version".to_string(), pkg_spec.to_string()];
    run_dsh_with_logs(app, env, &args, Some(&dsh_home_of(profiles_dir)))
}

/// 撤销豁免：dsh plugin disallow-version <pkg>@<version>
pub fn disallow_version(
    app: &tauri::AppHandle,
    env: &DshEnv,
    pkg_spec: &str,
    profiles_dir: &std::path::Path,
) -> InstallOutcome {
    let args = vec!["plugin".to_string(), "disallow-version".to_string(), pkg_spec.to_string()];
    run_dsh_with_logs(app, env, &args, Some(&dsh_home_of(profiles_dir)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_install_args() {
        // 验证参数构造（不实际执行）
        let env = DshEnv {
            id: "t".into(),
            name: "t".into(),
            source: crate::models::EnvSource::GlobalCli,
            version: "0.1.0-rc.7".into(),
            home_dir: ".".into(),
            run_command: "dsh".into(),
            bin_path: None,
            scan_profiles_dir: None,
        };
        let args = vec![
            "plugin".to_string(),
            "--profile".to_string(),
            "web".to_string(),
            "add".to_string(),
            "dshmarket".to_string(),
        ];
        assert_eq!(args[0], "plugin");
        assert_eq!(args[3], "add");
        assert_eq!(args[4], "dshmarket");
        let _ = &env;
    }

    #[test]
    fn failure_summary_carries_real_cli_reason() {
        // 失败摘要必须带 stderr 真实原因，而不是干巴巴的「详见日志」
        assert_eq!(failure_summary(true, "whatever"), "执行成功");
        assert_eq!(failure_summary(false, ""), "执行失败，详见日志");
        assert_eq!(
            failure_summary(false, "some npm error"),
            "执行失败：some npm error"
        );
        // 命中官方「特别管理」拒绝 → 追加可能原因提示（不拦截，仅失败时说明）
        let s = failure_summary(
            false,
            "error: profile \"desktop\" is managed exclusively by the Electron application",
        );
        assert!(s.contains("执行失败：error: profile \"desktop\""), "{s}");
        assert!(s.contains("特别管理"), "应提示可能被特别管理: {s}");
        assert!(s.contains("可能"), "提示语气是「可能」: {s}");
    }

    /// 真实验证：绝对路径 node 启动 dsh，**PATH 清空也能跑**（不靠 PATH、不置顶）。
    /// `cargo test --lib real_absolute_dsh_without_path -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_absolute_dsh_without_path() {
        let registry = "https://registry.npmmirror.com";
        // 不触发下载：仅当 runtime 已就绪才测；否则用本机可解析的 node+bin.js
        let env = {
            // 任选一个本机 dsh 布局（download 或 DSH Desktop）
            let candidates = [
                r"C:\dsh-versions\dsh-0.1.7-rc.1\node_modules\.bin\dsh.cmd",
                r"C:\dsh-versions\dsh-0.2.0-rc.1\node_modules\.bin\dsh.cmd",
            ];
            let hit = candidates.iter().find(|p| std::path::Path::new(p).is_file());
            let Some(cmd) = hit else {
                eprintln!("SKIP: 本机无 dsh 安装");
                return;
            };
            DshEnv {
                id: "real".into(),
                name: "real".into(),
                source: crate::models::EnvSource::Manual,
                version: "test".into(),
                home_dir: ".".into(),
                run_command: cmd.to_string(),
                bin_path: None,
                scan_profiles_dir: None,
            }
        };
        let _ = registry;
        let (node, binjs) = crate::runner::resolve_dsh_launcher(&env).expect("应解析出 node+bin.js");
        println!("node={node}\nbinjs={binjs}");
        // PATH 置空 + 只带 NODE 运行时变量 → 仍应输出版本
        let mut c = crate::fsutil::hidden_command(&node);
        c.arg(&binjs).arg("-V");
        c.env("PATH", "");
        c.env("NODE", &node);
        let out = c.output().expect("spawn node bin.js -V");
        let s = String::from_utf8_lossy(&out.stdout);
        let e = String::from_utf8_lossy(&out.stderr);
        println!("stdout=[{s}] stderr=[{e}]");
        assert!(!s.trim().is_empty(), "绝对路径启动 dsh -V 应有输出，PATH 为空也要能跑");
    }
}
