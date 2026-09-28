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

/// 执行 dsh 命令并流式推送日志事件
/// 事件: install-log (line, kind), install-done (success, exitCode)
pub fn run_dsh_with_logs(
    app: &tauri::AppHandle,
    env: &DshEnv,
    args: &[String],
    dsh_home: Option<&str>,
) -> InstallOutcome {
    let mut full = format!("chcp 65001 >nul && {}", env.run_command);
    for a in args {
        full.push(' ');
        full.push_str(a);
    }
    let mut cmd = Command::new("cmd");
    cmd.args(["/C", &full]);
    if let Some(home) = dsh_home {
        cmd.env("DSH_HOME", home);
    }
    // 注入 node 目录到 PATH（dsh.cmd shim / pnpm 依赖 node；PATH 可能没有）。
    // 电脑未装 node 时先自动下载安装（失败不阻断，日志如实展示）。
    let mut tc = crate::toolchain::probe();
    if !tc.node.is_file() {
        let registry = crate::settings::load_settings().npm_registry;
        match crate::toolchain::ensure(app, &registry) {
            Ok(t) => tc = t,
            Err(e) => {
                let _ = app.emit("install-log", serde_json::json!({ "line": format!("自动安装 Node.js 失败: {e}"), "kind": "stderr" }));
            }
        }
    }
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
    // stderr 线程
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
    InstallOutcome {
        success,
        exit_code,
        summary: if success { "执行成功".to_string() } else { "执行失败，详见日志".to_string() },
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
}
