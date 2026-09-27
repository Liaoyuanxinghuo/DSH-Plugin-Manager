//! 环境扫描器：探测 DSH 版本、解析 profile 与插件清单

use crate::models::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::time::SystemTime;

/// 执行一条 Windows shell 命令并返回输出（统一 UTF-8 代码页）
pub fn run_cmd(command: &str) -> CommandOutput {
    let full = format!("chcp 65001 >nul && {}", command);
    let output = Command::new("cmd")
        .args(["/C", &full])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW：禁止弹出控制台黑框
        .output();
    match output {
        Ok(o) => CommandOutput {
            success: o.status.success(),
            stdout: String::from_utf8_lossy(&o.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
            exit_code: o.status.code(),
        },
        Err(e) => CommandOutput {
            success: false,
            stdout: String::new(),
            stderr: format!("命令执行失败: {e}"),
            exit_code: None,
        },
    }
}

/// 获取默认 DSH_HOME（环境变量未设置时取 ~/.dsh）
pub fn default_dsh_home() -> PathBuf {
    if let Ok(h) = std::env::var("DSH_HOME") {
        if !h.trim().is_empty() {
            return PathBuf::from(h);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".dsh")
}

/// 探测全局安装的 dsh CLI（where dsh）
pub fn detect_global_cli() -> Option<DshEnv> {
    let where_out = run_cmd("where dsh");
    if !where_out.success {
        return None;
    }
    let first_line = where_out.stdout.lines().next()?.trim().to_string();
    if first_line.is_empty() {
        return None;
    }
    let version = probe_version("dsh");
    let home = default_dsh_home();
    Some(DshEnv {
        id: "global".to_string(),
        name: format!("全局 CLI (dsh {version})"),
        source: EnvSource::GlobalCli,
        version,
        home_dir: home.to_string_lossy().to_string(),
        run_command: "dsh".to_string(),
        bin_path: Some(first_line),
        scan_profiles_dir: None,
    })
}

/// 在指定目录中探测 dsh 本体可执行文件。
/// 候选位置：目录本身、目录/node_modules/.bin、目录/node_modules/@deepseek-ai/dsh 相关。
/// 返回 (显示命令, 版本)。
pub fn scan_dsh_binary(dir: &Path) -> Option<(String, String)> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    // 目录本身
    for name in ["dsh.cmd", "dsh.exe", "dsh.bat", "dsh"] {
        candidates.push(dir.join(name));
    }
    // node_modules/.bin
    for name in ["dsh.cmd", "dsh", "dsh.exe"] {
        candidates.push(dir.join("node_modules").join(".bin").join(name));
    }
    // 常见全局 npm 安装位置结构：dir/dsh.cmd（npm 全局目录本身就是）
    // 遍历一层子目录（覆盖 node_modules/@deepseek-ai/dsh 等布局）
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            // node_modules 内部：@scope 两层
            if name == "node_modules" {
                if let Ok(inner) = fs::read_dir(&p) {
                    for ie in inner.flatten() {
                        let ip = ie.path();
                        let iname = ie.file_name().to_string_lossy().to_string();
                        if iname.starts_with('@') {
                            // scoped 包目录
                            if let Ok(inner2) = fs::read_dir(&ip) {
                                for iie in inner2.flatten() {
                                    let iip = iie.path();
                                    if iip.join(".bin").is_dir() {
                                        candidates.push(iip.join(".bin").join("dsh.cmd"));
                                        candidates.push(iip.join(".bin").join("dsh"));
                                    }
                                }
                            }
                        } else if ip.join(".bin").is_dir() {
                            candidates.push(ip.join(".bin").join("dsh.cmd"));
                            candidates.push(ip.join(".bin").join("dsh"));
                        }
                    }
                }
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    for cand in candidates {
        let key = cand.to_string_lossy().to_string();
        if !seen.insert(key.clone()) {
            continue;
        }
        if !cand.exists() {
            continue;
        }
        let version = probe_version(&key);
        if version != "unknown" {
            return Some((key, version));
        }
    }
    None
}

/// 递归（有限深度）扫描目录树，寻找所有 dsh 本体
pub fn scan_dsh_binary_tree(root: &Path, max_depth: usize) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut visited = std::collections::HashSet::new();
    scan_dir_recursive(root, 0, max_depth, &mut found, &mut visited);
    // 去重
    let mut seen = std::collections::HashSet::new();
    found.retain(|(cmd, _)| seen.insert(cmd.clone()));
    found
}

fn scan_dir_recursive(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    found: &mut Vec<(String, String)>,
    visited: &mut std::collections::HashSet<PathBuf>,
) {
    if depth > max_depth {
        return;
    }
    // node_modules 内部结构由 scan_dsh_binary 通过 .bin 探测，递归跳过避免爆炸
    if dir.file_name().map(|n| n.to_string_lossy() == "node_modules").unwrap_or(false) {
        return;
    }
    if !visited.insert(dir.to_path_buf()) {
        return;
    }
    // 先探测当前目录
    if let Some(hit) = scan_dsh_binary(dir) {
        found.push(hit);
    }
    // 递归子目录（限制数量避免爆炸）
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut subdirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    subdirs.sort();
    // 最多处理 64 个子目录，避免扫描爆炸
    for sub in subdirs.into_iter().take(64) {
        scan_dir_recursive(&sub, depth + 1, max_depth, found, visited);
    }
}

/// 探测指定命令的 dsh 版本
pub fn probe_version(command: &str) -> String {
    let out = run_cmd(&format!("{command} --version"));
    // 输出可能是 "dsh 0.1.0-rc.7" 或直接版本号，取第一个形如 x.y.z 的 token
    for line in out.stdout.lines() {
        for tok in line.split_whitespace() {
            if is_version_like(tok) {
                return tok.to_string();
            }
        }
    }
    for line in out.stderr.lines() {
        for tok in line.split_whitespace() {
            if is_version_like(tok) {
                return tok.to_string();
            }
        }
    }
    // 兜底：命令执行失败时（如 PATH 中无 node），从可执行文件相邻的
    // node_modules/@deepseek-ai/dsh/package.json 读取版本（覆盖 DSH Desktop 内置目录等布局）
    let p = Path::new(command);
    if let Some(dir) = p.parent() {
        let mut candidates = vec![
            dir.join("node_modules").join("@deepseek-ai").join("dsh").join("package.json"),
            dir.join("package.json"),
        ];
        // 命令可能带参数（如 "node xxx/bin.js"），取第一个路径段
        if command.contains(' ') {
            if let Some(first) = command.split_whitespace().next() {
                let p2 = Path::new(first);
                if let Some(d2) = p2.parent() {
                    candidates.push(d2.join("node_modules").join("@deepseek-ai").join("dsh").join("package.json"));
                }
            }
        }
        for cand in candidates {
            if let Ok(raw) = fs::read_to_string(&cand) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Some(v) = json.get("version").and_then(|v| v.as_str()) {
                        if !v.is_empty() {
                            return v.to_string();
                        }
                    }
                }
            }
        }
    }
    "unknown".to_string()
}

fn is_version_like(s: &str) -> bool {
    let t = s.trim_start_matches(['v', 'V']);
    t.chars().next().is_some_and(|c| c.is_ascii_digit())
        && t.contains('.')
}

/// 扫描所有可用的 DSH 环境（全局 CLI + 手动添加 + 扫描目录）
pub fn scan_envs(manual_envs: &[DshEnv], scan_dirs: &[ScanDirEntry]) -> Vec<DshEnv> {
    let mut envs: Vec<DshEnv> = Vec::new();
    if let Some(g) = detect_global_cli() {
        envs.push(g);
    }
    // 手动环境去重追加
    for m in manual_envs {
        if !envs.iter().any(|e| e.id == m.id) {
            envs.push(m.clone());
        }
    }
    // 扫描目录不再作为"环境"出现在左栏（它不是 dsh 程序）；
    // 其 profiles 由 list_all_profiles 合并到中栏统一展示。
    envs
}

/// 校验手动添加的环境：执行 --version 确认可用，返回版本号
pub fn validate_manual_env(command: &str) -> Result<String, String> {
    if command.trim().is_empty() {
        return Err("命令不能为空".to_string());
    }
    let out = run_cmd(&format!("{command} --version"));
    if !out.success && out.stderr.is_empty() {
        return Err(format!("命令执行失败: {}", out.stderr));
    }
    let version = probe_version(command);
    if version == "unknown" {
        return Err(format!(
            "无法从输出中识别版本号（stdout: {} | stderr: {}）",
            out.stdout, out.stderr
        ));
    }
    Ok(version)
}

/// 列出某个 DSH_HOME 下的全部 profile
pub fn list_profiles(home_dir: &str) -> Vec<ProfileInfo> {
    list_profiles_from(&Path::new(home_dir).join("profiles"))
}

/// 直接扫描指定 profiles 目录
pub fn list_profiles_from(profiles_dir: &Path) -> Vec<ProfileInfo> {
    let mut result = Vec::new();
    let entries = match fs::read_dir(profiles_dir) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // 跳过非 profile 目录（node_modules、.generations 等隐藏目录）
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        let (plugin_count, bundles) = parse_profile_package(&path);
        result.push(ProfileInfo {
            name,
            profiles_dir: profiles_dir.to_string_lossy().to_string(),
            path: path.to_string_lossy().to_string(),
            plugin_count,
            data_size: dir_size(&path),
            modified: fmt_modified(&path),
            has_patch: path.join("cordis.patch.yml").exists(),
            has_lock: path.join("pnpm-lock.yaml").exists(),
            has_package: path.join("package.json").exists(),
        });
        let _ = bundles;
    }
    // 按名称排序
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

/// 解析 profile 目录下的 package.json
pub fn parse_profile_package(profile_dir: &Path) -> (usize, Vec<String>) {
    let pkg_path = profile_dir.join("package.json");
    let raw = match fs::read_to_string(&pkg_path) {
        Ok(s) => s,
        Err(_) => return (0, Vec::new()),
    };
    let json: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return (0, Vec::new()),
    };
    let deps = json
        .get("dependencies")
        .and_then(|d| d.as_object())
        .map(|m| m.len())
        .unwrap_or(0);
    let bundles = json
        .get("dsh")
        .and_then(|d| d.get("profile"))
        .and_then(|p| p.get("bundles"))
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    (deps, bundles)
}

/// 读取 profile 的插件清单（完整结构）
pub fn read_profile_package_full(profile_dir: &Path) -> Option<ProfilePackage> {
    let pkg_path = profile_dir.join("package.json");
    let raw = fs::read_to_string(&pkg_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let deps = json
        .get("dependencies")
        .and_then(|d| d.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect::<std::collections::HashMap<_, _>>()
        })
        .unwrap_or_default();
    let bundles = json
        .get("dsh")
        .and_then(|d| d.get("profile"))
        .and_then(|p| p.get("bundles"))
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(ProfilePackage {
        name: json.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
        dependencies: deps,
        bundles,
    })
}

/// 计算目录大小（递归）
pub fn dir_size(path: &Path) -> u64 {
    let mut total: u64 = 0;
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            total += dir_size(&p);
        } else if let Ok(md) = fs::metadata(&p) {
            total += md.len();
        }
    }
    total
}

fn fmt_modified(path: &Path) -> String {
    let md = match fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return String::new(),
    };
    let modified: SystemTime = match md.modified() {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    match modified.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs();
            format_epoch(secs)
        }
        Err(_) => String::new(),
    }
}

fn format_epoch(secs: u64) -> String {
    // 使用本地时间格式化，简化实现：转成 (年-月-日 时:分) 字符串
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m) = (rem / 3600, (rem % 3600) / 60);
    // 1970-01-01 起算，近似日历（忽略闰秒，使用儒略日换算）
    let jdn = days as i64 + 2440588;
    let (y, mo, d) = jdn_to_date(jdn);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", y, mo, d, h, m)
}

fn jdn_to_date(jdn: i64) -> (i64, i64, i64) {
    let a = jdn + 32044;
    let b = (4 * a + 3) / 146097;
    let c = a - 146097 * b / 4;
    let d = (4 * c + 3) / 1461;
    let e = c - 1461 * d / 4;
    let m = (5 * e + 2) / 153;
    let day = e - (153 * m + 2) / 5 + 1;
    let month = m + 3 - 12 * (m / 10);
    let year = 100 * b + d - 4800 + m / 10;
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_like() {
        assert!(is_version_like("0.1.7-rc.2"));
        assert!(is_version_like("v1.2.3"));
        assert!(!is_version_like("dsh"));
        assert!(!is_version_like("unknown"));
    }

    #[test]
    fn test_jdn_conversion() {
        // 已知基准：JDN 2451545 = 2000-01-01
        let (y, mo, d) = jdn_to_date(2451545);
        assert_eq!((y, mo, d), (2000, 1, 1));
        // 2000-01-01 起 9497 天 = 2026-01-01（含 7 个闰日）
        let (y, mo, d) = jdn_to_date(2451545 + 9497);
        assert_eq!((y, mo, d), (2026, 1, 1));
    }

    #[test]
    fn test_run_cmd_echo() {
        let out = run_cmd("echo hello-test");
        assert!(out.success);
        assert!(out.stdout.contains("hello-test"));
    }

    #[test]
    fn test_scan_dsh_binary_fake() {
        // 模拟一个假 dsh.cmd 本体
        let work = std::env::temp_dir().join(format!("dshpm-bin-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).unwrap();
        let script = work.join("dsh.cmd");
        fs::write(&script, "@echo off\r\necho dsh 9.9.9-fake\r\n").unwrap();

        let hit = scan_dsh_binary(&work);
        assert!(hit.is_some(), "应找到 dsh.cmd");
        let (cmd, ver) = hit.unwrap();
        assert!(cmd.contains("dsh.cmd"));
        assert_eq!(ver, "9.9.9-fake");

        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn test_scan_dsh_binary_node_modules_bin() {
        // 模拟 node_modules/.bin/dsh.cmd 布局
        let work = std::env::temp_dir().join(format!("dshpm-bin2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        let bin = work.join("node_modules").join(".bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("dsh.cmd"), "@echo off\r\necho dsh 1.2.3-rc.4\r\n").unwrap();

        let hit = scan_dsh_binary(&work);
        assert!(hit.is_some(), "应通过 node_modules/.bin 找到");
        let (_, ver) = hit.unwrap();
        assert_eq!(ver, "1.2.3-rc.4");

        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn test_scan_dsh_binary_tree_real_dshwin() {
        // 真实环境验证：DSH Desktop 内置目录（若本机装有）
        let d = Path::new("C:/Users/xiaolei/.dsh-win/versions/0.1.7-rc.2-mc");
        if !d.is_dir() {
            eprintln!("SKIP: DSH Desktop 目录不存在");
            return;
        }
        let found = scan_dsh_binary_tree(d, 3);
        eprintln!("扫描结果: {:?}", found);
        assert!(!found.is_empty(), "DSH Desktop 目录应发现 dsh 本体");
    }

    #[test]
    fn test_probe_version_pkg_fallback() {
        // 命令执行失败时（如 PATH 无 node），应从相邻 package.json 兜底读版本
        let work = std::env::temp_dir().join(format!("dshpm-pkgfb-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        let pkg_dir = work.join("node_modules").join("@deepseek-ai").join("dsh");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(
            pkg_dir.join("package.json"),
            "{\"name\":\"@deepseek-ai/dsh\",\"version\":\"8.8.8-fallback\"}",
        )
        .unwrap();
        // 命令指向不存在的可执行文件 → 命令探测失败 → 走 package.json 兜底
        let v = probe_version(&work.join("dsh.cmd").to_string_lossy());
        assert_eq!(v, "8.8.8-fallback");
        let _ = fs::remove_dir_all(&work);
    }

    #[test]
    fn test_scan_dsh_binary_tree_real_global() {
        // 真实环境验证：全局 npm 目录应能找到 dsh 本体（若本机装有）
        let global_dir = dirs::home_dir()
            .unwrap()
            .join("AppData")
            .join("Roaming")
            .join("npm");
        if !global_dir.is_dir() {
            eprintln!("SKIP: 全局 npm 目录不存在");
            return;
        }
        let found = scan_dsh_binary_tree(&global_dir, 3);
        assert!(!found.is_empty(), "全局 npm 目录应至少发现一个 dsh 本体");
        eprintln!("发现 dsh 本体: {:?}", found);
    }
}
