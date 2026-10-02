//! DSH Plugin Manager 应用入口与命令注册

mod browsers;
mod buildpermit;
mod dsh_install;
mod fsutil;
mod ghnet;
mod installer;
mod market;
mod models;
mod npm;
mod health;
mod packforge;
mod patchfile;
mod settings;
mod toolchain;
mod profile_io;
mod runner;
mod scanner;
mod webauth;

use models::*;
use std::collections::HashMap;
use tauri::Manager;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::State;
use tauri::Emitter;

/// 应用全局状态
pub struct AppState {
    /// 手动添加的 DSH 环境（持久化）
    pub manual_envs: Mutex<Vec<DshEnv>>,
    /// 用户添加的本地 profile 扫描目录（持久化）
    pub scan_dirs: Mutex<Vec<ScanDirEntry>>,
    /// 运行中的 DSH 进程（env_id -> process）
    pub running: Mutex<HashMap<String, RunningProcess>>,
    /// 正在启动中的组合 key（per-key 互斥，防并发双击双开/抢端口）
    pub starting: Mutex<std::collections::HashSet<String>>,
}

/// 手动环境持久化文件路径
fn manual_envs_file() -> PathBuf {
    let dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.join("dsh-plugin-manager").join("envs.json")
}

/// 扫描目录持久化文件路径
fn scan_dirs_file() -> PathBuf {
    let dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.join("dsh-plugin-manager").join("scan_dirs.json")
}

fn load_scan_dirs() -> Vec<ScanDirEntry> {
    let path = scan_dirs_file();
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_scan_dirs(entries: &[ScanDirEntry]) {
    let path = scan_dirs_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string(entries) {
        let _ = std::fs::write(path, s);
    }
}

fn load_manual_envs() -> Vec<DshEnv> {
    let path = manual_envs_file();
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_manual_envs(envs: &[DshEnv]) {
    let path = manual_envs_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string(envs) {
        let _ = std::fs::write(path, s);
    }
}

// ==================== 命令：环境 ====================

/// 扫描全部 DSH 环境（全局 CLI + 手动 + 扫描目录）
#[tauri::command]
async fn scan_envs(state: State<'_, AppState>) -> Result<Vec<DshEnv>, String> {
    let manual = state.manual_envs.lock().unwrap().clone();
    let scans = state.scan_dirs.lock().unwrap().clone();
    // 目录扫描放阻塞线程池，避免占用 tokio worker
    tauri::async_runtime::spawn_blocking(move || scanner::scan_envs(&manual, &scans))
        .await
        .map_err(|e| format!("扫描失败：{e}"))
}

/// 手动添加环境（如 pnpm dlx 版本）
/// validate_manual_env 会执行命令探测版本（spawn 子进程）→ 阻塞线程池。
#[tauri::command]
async fn add_manual_env(
    state: State<'_, AppState>,
    name: String,
    command: String,
) -> Result<DshEnv, String> {
    let name = name.trim().to_string();
    let command = command.trim().to_string();
    if name.is_empty() || command.is_empty() {
        return Err("名称和命令都不能为空".to_string());
    }
    let version = {
        let command = command.clone();
        tauri::async_runtime::spawn_blocking(move || scanner::validate_manual_env(&command))
            .await
            .map_err(|e| format!("探测 dsh 版本失败: {e}"))??
    };
    let id = format!("manual-{}", uuid_like(&command));
    let env = DshEnv {
        id,
        name: format!("{name} (dsh {version})"),
        source: EnvSource::Manual,
        version,
        home_dir: scanner::default_dsh_home().to_string_lossy().to_string(),
        run_command: command,
        bin_path: None,
        scan_profiles_dir: None,
    };
    let mut envs = state.manual_envs.lock().unwrap();
    envs.push(env.clone());
    save_manual_envs(&envs);
    Ok(env)
}

/// 添加本地 profile 扫描目录（FS 检查/写文件 → 阻塞线程池）
#[tauri::command]
async fn add_scan_dir(state: State<'_, AppState>, path: String) -> Result<ScanDirEntry, String> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("目录路径不能为空".to_string());
    }
    let entry = tauri::async_runtime::spawn_blocking(move || {
        let p = std::path::Path::new(&path);
        if !p.is_dir() {
            return Err(format!("目录不存在或不可访问: {path}"));
        }
        // 解析实际 profiles 目录
        let profiles_dir = resolve_profiles_dir(p)?;
        let id = format!("scan-{}", uuid_like(&path));
        let label = std::path::Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        Ok(ScanDirEntry {
            id,
            path,
            label,
            profiles_dir: profiles_dir.to_string_lossy().to_string(),
            dsh_bin: None,
        })
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))??;
    let mut dirs = state.scan_dirs.lock().unwrap();
    if dirs.iter().any(|d| d.path == entry.path) {
        return Err("该目录已在扫描列表中".to_string());
    }
    dirs.push(entry.clone());
    save_scan_dirs(&dirs);
    Ok(entry)
}

/// 解析用户输入的路径 → 实际 profiles 目录
fn resolve_profiles_dir(p: &std::path::Path) -> Result<std::path::PathBuf, String> {
    // 1) 本身就是 DSH_HOME（含 profiles 子目录）
    let profiles_sub = p.join("profiles");
    if profiles_sub.is_dir() {
        return Ok(profiles_sub);
    }
    // 2) 本身就是 profiles 目录（含至少一个 profile 子目录：目录下有 package.json 或任意子目录）
    if is_profiles_dir(p) {
        return Ok(p.to_path_buf());
    }
    // 3) 本身是单个 profile 目录（含 package.json）
    if p.join("package.json").is_file() {
        return p
            .parent()
            .map(|pp| pp.to_path_buf())
            .ok_or_else(|| "无法解析 profile 上级目录".to_string());
    }
    Err(format!(
        "无法识别目录结构：{}（需为 DSH_HOME、profiles 目录或单个 profile 目录）",
        p.display()
    ))
}

/// 判断目录是否像 profiles 目录（存在至少一个子目录且子目录含 package.json，或存在多个子目录）
fn is_profiles_dir(p: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(p) else {
        return false;
    };
    let mut dir_count = 0;
    let mut pkg_count = 0;
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }
        dir_count += 1;
        if path.join("package.json").is_file() {
            pkg_count += 1;
        }
    }
    dir_count >= 1 && (pkg_count >= 1 || dir_count >= 2)
}

/// 删除扫描目录（FS 写 → 阻塞线程池）
#[tauri::command]
async fn remove_scan_dir(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let snapshot = {
        let mut dirs = state.scan_dirs.lock().unwrap();
        dirs.retain(|d| d.id != id);
        dirs.clone()
    };
    tauri::async_runtime::spawn_blocking(move || save_scan_dirs(&snapshot))
        .await
        .map_err(|e| format!("后台任务失败: {e}"))?;
    Ok(())
}

/// 列出所有扫描目录（纯内存快照，无 IO）
#[tauri::command]
async fn list_scan_dirs(state: State<'_, AppState>) -> Result<Vec<ScanDirEntry>, String> {
    Ok(state.scan_dirs.lock().unwrap().clone())
}

/// 在指定目录中扫描 dsh 本体并添加为可运行环境（自动探测命令与版本）
#[tauri::command]
async fn add_dsh_scan_dir(state: State<'_, AppState>, path: String) -> Result<Vec<DshEnv>, String> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("目录路径不能为空".to_string());
    }
    let p = std::path::Path::new(&path);
    if !p.is_dir() {
        return Err(format!("目录不存在或不可访问: {path}"));
    }
    // 递归扫描大目录放阻塞线程池，避免卡 UI
    let scan_root = p.to_path_buf();
    let found = tauri::async_runtime::spawn_blocking(move || scanner::scan_dsh_binary_tree(&scan_root, 3))
        .await
        .map_err(|e| format!("扫描失败：{e}"))?;
    if found.is_empty() {
        return Err(format!("未在目录中发现 dsh 本体：{path}"));
    }
    let label = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());

    let mut envs = state.manual_envs.lock().unwrap();
    let mut added: Vec<DshEnv> = Vec::new();
    for (cmd, version) in found {
        // 与现有环境（含全局）去重
        if envs.iter().any(|e| e.run_command == cmd) {
            continue;
        }
        let id = format!("manual-{}", uuid_like(&cmd));
        let env = DshEnv {
            id,
            name: format!("扫描: {label} (dsh {version})"),
            source: EnvSource::Manual,
            version,
            home_dir: scanner::default_dsh_home().to_string_lossy().to_string(),
            run_command: cmd.clone(),
            bin_path: Some(cmd),
            scan_profiles_dir: None,
        };
        envs.push(env.clone());
        added.push(env);
    }
    drop(envs);
    if added.is_empty() {
        return Err("发现的 dsh 本体均已存在，无需重复添加".to_string());
    }
    save_manual_envs_from_state(&state);
    Ok(added)
}

fn save_manual_envs_from_state(state: &State<'_, AppState>) {
    let envs = state.manual_envs.lock().unwrap();
    save_manual_envs(&envs);
}

/// 从 runCommand / binPath 向上解析「版本根目录」：
/// 第一个目录名以 "dsh-" 开头且包含版本号的目录（如 dsh-0.1.7-rc.1）。
fn version_root_of(env: &DshEnv) -> Option<std::path::PathBuf> {
    let start = env
        .run_command
        .split_whitespace()
        .next()
        .map(|s| s.to_string())
        .or_else(|| env.bin_path.clone())?;
    let p = std::path::PathBuf::from(&start);
    let mut dir = if p.is_dir() {
        Some(p)
    } else {
        p.parent().map(|d| d.to_path_buf())
    };
    let v = env.version.trim().trim_start_matches('v').to_string();
    while let Some(d) = dir {
        let name = d
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.starts_with("dsh-") && (v.is_empty() || name.contains(&v)) {
            return Some(d);
        }
        dir = d.parent().map(|d| d.to_path_buf());
    }
    None
}

/// 磁盘删除安全验证（四重防线，全部满足才允许删除）：
/// 1) 目标不是下载根自身、也不包含下载根；
/// 2) 目标位于下载根之内（仅删除下载根下的版本目录）；
/// 3) 目录名以 "dsh-" 开头；
/// 4) 目录具备 DSH 安装特征（package.json / node_modules\.bin\dsh.cmd / @deepseek-ai\dsh\package.json）。
fn safe_to_delete_version(root: &std::path::Path, download_root: &std::path::Path) -> bool {
    if root == download_root || download_root.starts_with(root) {
        return false;
    }
    if !root.starts_with(download_root) {
        return false;
    }
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if !name.starts_with("dsh-") {
        return false;
    }
    let has_pkg = root.join("package.json").is_file();
    let has_bin = root.join("node_modules").join(".bin").join("dsh.cmd").is_file();
    let has_dep = root
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("package.json")
        .is_file();
    has_pkg || has_bin || has_dep
}

/// 删除手动添加的环境：从磁盘删除对应 DSH 版本目录（安全验证通过时），再移出列表。
/// 返回删除结果说明；仅在验证不满足时才仅移出列表并明确说明。
#[tauri::command]
async fn remove_manual_env(state: State<'_, AppState>, id: String) -> Result<String, String> {
    // 锁包在独立作用域：guard 在块尾自动释放（MutexGuard 非 Send，跨 await 会让 future 非 Send）
    let env = {
        let envs = state.manual_envs.lock().unwrap();
        envs.iter().find(|e| e.id == id).cloned()
    };
    let Some(env) = env else {
        return Err("环境不存在".to_string());
    };

    // 运行中拒绝删除（进程占用会删失败或留下半删除状态）
    let running = state
        .running
        .lock()
        .unwrap()
        .values()
        .any(|p| p.env_id == id);
    if running {
        return Err("该 DSH 版本正在运行，请先停止相关实例再删除".to_string());
    }

    // ===== 先从列表移除（立即生效；用户意图是移除该版本，不依赖磁盘删除结果）=====
    state.manual_envs.lock().unwrap().retain(|e| e.id != id);
    {
        let envs_now = state.manual_envs.lock().unwrap();
        save_manual_envs(&envs_now);
    }

    // ===== 再尝试删除磁盘目录（尽力而为；失败仅提示，不影响列表移除）=====
    let mut disk_msg = String::from("已从列表移除该环境（未发现可安全删除的版本目录，磁盘文件未动）");
    if let Some(root) = version_root_of(&env) {
        let dl = settings::load_settings().dsh_download_dir;
        let dl_root = std::path::PathBuf::from(if dl.trim().is_empty() {
            settings::DEFAULT_DSH_DIR
        } else {
            &dl
        });
        let dl_default = std::path::PathBuf::from(settings::DEFAULT_DSH_DIR);
        if safe_to_delete_version(&root, &dl_root) || safe_to_delete_version(&root, &dl_default) {
            // 大目录删除放阻塞线程池
            let root2 = root.clone();
            let rm = tauri::async_runtime::spawn_blocking(move || std::fs::remove_dir_all(&root2))
                .await
                .map_err(|e| format!("版本目录删除任务失败：{e}"))?;
            match rm {
                Ok(_) => {
                    disk_msg = format!("已删除磁盘上的版本目录：{}", root.display());
                }
                Err(e) => {
                    disk_msg = format!(
                        "已从列表移除该环境；磁盘目录删除失败（{}）：{e}，可稍后手动删除该目录",
                        root.display()
                    );
                }
            }
        } else {
            disk_msg = format!(
                "已从列表移除该环境（版本目录 {} 不在受管下载根内，为避免误删未动磁盘）",
                root.display()
            );
        }
    }
    Ok(disk_msg)
}

/// per-key 启动互斥的 RAII 守卫：离开作用域（含提前 return）时自动释放
struct StartLockGuard<'a> {
    key: String,
    state_starting: &'a Mutex<std::collections::HashSet<String>>,
}
impl Drop for StartLockGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut set) = self.state_starting.lock() {
            set.remove(&self.key);
        }
    }
}

fn uuid_like(s: &str) -> String {
    // 用简单哈希生成短 id，避免引入额外依赖
    let mut h: u64 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u64);
    }
    format!("{:016x}", h)
}

/// 获取指定环境
#[tauri::command]
async fn get_env(state: State<'_, AppState>, env_id: String) -> Result<Option<DshEnv>, String> {
    // 快路径：手动环境已在 state，直接命中（免扫描）
    {
        let manual = state.manual_envs.lock().unwrap();
        if let Some(e) = manual.iter().find(|e| e.id == env_id) {
            return Ok(Some(e.clone()));
        }
    }
    // 兜底 scan_envs（spawn 探测版本）→ 阻塞线程池；锁不跨 await
    let manual = state.manual_envs.lock().unwrap().clone();
    let scans = state.scan_dirs.lock().unwrap().clone();
    let found = tauri::async_runtime::spawn_blocking(move || {
        scanner::scan_envs(&manual, &scans)
            .into_iter()
            .find(|e| e.id == env_id)
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?;
    Ok(found)
}

// ==================== 命令：Profile ====================

/// 合并逻辑（可测试）：默认 DSH_HOME/profiles + 所有扫描目录，按来源目录去重
fn list_all_profiles_inner(scans: &[ScanDirEntry]) -> Vec<ProfileInfo> {
    let mut all: Vec<ProfileInfo> = Vec::new();
    let home = scanner::default_dsh_home().join("profiles");
    let mut seen = std::collections::HashSet::new();
    if home.is_dir() {
        seen.insert(home.to_string_lossy().to_string());
        all.extend(scanner::list_profiles_from(&home));
    }
    for s in scans {
        let pd = std::path::PathBuf::from(&s.profiles_dir);
        let key = pd.to_string_lossy().to_string();
        if !seen.insert(key) {
            continue;
        }
        if !pd.is_dir() {
            continue;
        }
        all.extend(scanner::list_profiles_from(&pd));
    }
    all.sort_by(|a, b| a.name.cmp(&b.name));
    all
}

/// 合并列出全部可用 profile（默认 DSH_HOME/profiles + 所有扫描目录），每个带来源目录
#[tauri::command]
async fn list_all_profiles(state: State<'_, AppState>) -> Result<Vec<ProfileInfo>, String> {
    let scans = state.scan_dirs.lock().unwrap().clone();
    tauri::async_runtime::spawn_blocking(move || list_all_profiles_inner(&scans))
        .await
        .map_err(|e| format!("扫描 profiles 失败: {e}"))
}

/// 增量扫描：只列单个 profiles 目录（path 空 = 默认 DSH_HOME/profiles）。
/// 供前端「扫到一个目录就先显示一个」。
#[tauri::command]
async fn list_profiles_from_dir(path: String) -> Result<Vec<ProfileInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = if path.trim().is_empty() {
            scanner::default_dsh_home().join("profiles")
        } else {
            std::path::PathBuf::from(path.trim())
        };
        if !p.is_dir() {
            return Ok(Vec::new());
        }
        Ok(scanner::list_profiles_from(&p))
    })
    .await
    .map_err(|e| format!("扫描 profiles 失败: {e}"))?
}

/// 逐条流式扫描 profiles：扫到一个就 emit 一个 `profile-found`，全部结束 emit `profiles-scan-done`。
#[tauri::command]
async fn scan_profiles_live(app: tauri::AppHandle, paths: Vec<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        for path in paths {
            let p = if path.trim().is_empty() {
                scanner::default_dsh_home().join("profiles")
            } else {
                std::path::PathBuf::from(path.trim())
            };
            if !p.is_dir() {
                continue;
            }
            scanner::list_profiles_from_each(&p, |info| {
                let _ = app.emit("profile-found", &info);
            });
        }
        let _ = app.emit("profiles-scan-done", true);
        Ok(())
    })
    .await
    .map_err(|e| format!("扫描 profiles 失败: {e}"))?
}

/// 列出某环境的全部 profile
#[tauri::command]
async fn list_profiles(state: State<'_, AppState>, env_id: String) -> Result<Vec<ProfileInfo>, String> {
    let env = get_env_inner(&state, &env_id)?;
    if let Some(spd) = &env.scan_profiles_dir {
        Ok(scanner::list_profiles_from(std::path::Path::new(spd)))
    } else {
        Ok(scanner::list_profiles(&env.home_dir))
    }
}

fn get_env_inner(state: &State<'_, AppState>, env_id: &str) -> Result<DshEnv, String> {
    // 快路径：手动/扫描目录环境已在 state 中，避免每次全盘 scan_envs（会探测版本，卡 UI）
    {
        let manual = state.manual_envs.lock().unwrap();
        if let Some(e) = manual.iter().find(|e| e.id == env_id) {
            return Ok(e.clone());
        }
    }
    let scans = state.scan_dirs.lock().unwrap().clone();
    scanner::scan_envs(&state.manual_envs.lock().unwrap(), &scans)
        .into_iter()
        .find(|e| e.id == env_id)
        .ok_or_else(|| format!("环境不存在: {env_id}"))
}

/// 读取 profile 的插件清单（含 dump-config，耗时）→ 阻塞线程池
#[tauri::command]
async fn list_plugins(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<Vec<PluginInfo>, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    tauri::async_runtime::spawn_blocking(move || {
        let profile_dir = profiles_dir.join(&profile);
        if !profile_dir.exists() {
            return Err(format!("profile 不存在: {profile}"));
        }
        let pkg = scanner::read_profile_package_full(&profile_dir)
            .ok_or_else(|| "无法读取 profile 的 package.json".to_string())?;
        // 官方语义：按「条目 id + 模块名称」匹配 disabled；服务 id 以包 bundle patch 为准
        let patch_content = std::fs::read_to_string(profile_dir.join("cordis.patch.yml")).unwrap_or_default();
        let service_map = load_service_map(&env, &profile);
        let mut plugins: Vec<PluginInfo> = pkg
            .dependencies
            .into_iter()
            .map(|(name, spec)| {
                let is_bundle = pkg.bundles.contains(&name);
                // market 语义：unbundled（有 dsh.bundle 但不在 bundles）= 没加载 = 关；
                // 行禁用用 some()（任一 insert 行禁用就算禁用），不用 all()
                let unbundled = is_unbundled(&profile_dir, &name, is_bundle);
                let entries = service_entries_for_plugin(&profile_dir, &service_map, &name);
                let rows_off = !entries.is_empty()
                    && entries
                        .iter()
                        .any(|(id, mod_name)| patchfile::is_plugin_disabled(&patch_content, id, mod_name.as_deref()));
                let is_disabled = unbundled || rows_off;
                PluginInfo {
                    name,
                    spec,
                    is_bundle,
                    compatible: None,
                    incompatible_reason: None,
                    is_disabled,
                }
            })
            .collect();
        plugins.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(plugins)
    })
    .await
    .map_err(|e| format!("加载插件列表失败: {e}"))?
}

/// 读取 profile 的 cordis.patch.yml 中禁用的插件 id
/// 执行 dsh --dump-config 获取「包名 → 服务 id」映射（仅可运行环境）
fn load_service_map(env: &DshEnv, profile: &str) -> HashMap<String, String> {
    if env.run_command.trim().is_empty() {
        return HashMap::new();
    }
    let out = scanner::run_cmd(&format!("{} --profile {} --dump-config", env.run_command, profile));
    if !out.success {
        return HashMap::new();
    }
    patchfile::parse_service_ids(&out.stdout)
}

/// 插件名 → 服务 id：优先 dump-config 映射，其次包名推导
fn service_id_for(service_map: &HashMap<String, String>, package_name: &str) -> String {
    service_map
        .get(package_name)
        .cloned()
        .unwrap_or_else(|| patchfile::derive_service_id(package_name))
}

/// 读取已安装包自己的 bundle patch，解析其 insert 的真实服务 id / name 列表。
/// 这是官方权威来源（如 dshmarket → dsh-market、@hyzyn/dsh-mcp → mcp-config）；
/// name 是覆盖项声明的模块名（多行包子路径，如 @michengai/dsh-codex-ui/session-title），
/// **不能一律用包名**，否则会匹配到别的条目。
fn bundle_insert_entries(
    profile_dir: &std::path::Path,
    package_name: &str,
) -> Vec<(String, Option<String>)> {
    let pkg_dir = profile_dir.join("node_modules").join(package_name);
    let manifest_path = pkg_dir.join("package.json");
    let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let patch_rel = json
        .get("dsh")
        .and_then(|d| d.get("bundle"))
        .and_then(|b| b.get("patch"))
        .and_then(|p| p.as_str())
        .unwrap_or("cordis.patch.yml");
    let patch_path = pkg_dir.join(patch_rel.trim_start_matches("./"));
    let Ok(content) = std::fs::read_to_string(&patch_path) else {
        return Vec::new();
    };
    patchfile::parse_insert_entries(&content)
}

/// 某插件的全部候选 (服务 id, 模块名)（去重、保序）：
/// 1. 包 bundle patch 的 insert 条目（权威；name 用声明的模块名）
/// 2. dump-config 映射 / 包名推导（仅当无 insert 条目时兜底）
fn service_entries_for_plugin(
    profile_dir: &std::path::Path,
    service_map: &HashMap<String, String>,
    package_name: &str,
) -> Vec<(String, Option<String>)> {
    let mut entries = bundle_insert_entries(profile_dir, package_name);
    // market `rowIdsForPackage`：有 insert 行就只用 insert 行，不加幻影 fallback
    // （fallback id 查不到条目 → all() 永假 → 显示「已启用」但实际已禁用）
    if entries.is_empty() {
        let fallback_id = service_id_for(service_map, package_name);
        entries.push((fallback_id, Some(package_name.to_string())));
    }
    entries
}

/// 包是否声明了 dsh.bundle（组合包）
fn declares_bundle(profile_dir: &std::path::Path, package_name: &str) -> bool {
    let manifest_path = profile_dir.join("node_modules").join(package_name).join("package.json");
    let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    json.get("dsh").and_then(|d| d.get("bundle")).is_some()
}

/// market `unbundled`：声明了 dsh.bundle 但不在 `dsh.profile.bundles` → 没被加载
fn is_unbundled(profile_dir: &std::path::Path, package_name: &str, in_bundles: bool) -> bool {
    !in_bundles && declares_bundle(profile_dir, package_name)
}
fn read_disabled_ids(profile_dir: &std::path::Path) -> Vec<String> {
    let patch_path = profile_dir.join("cordis.patch.yml");
    match std::fs::read_to_string(&patch_path) {
        Ok(content) => patchfile::list_disabled_ids(&content),
        Err(_) => Vec::new(),
    }
}

/// 设置插件启用/停用。
/// market #696 B：**两层同写** —— patch 行 `disabled` + `dsh.profile.bundles`，
/// 只写一层就会「开关不同步」。inbox bundle 不动 bundles；带 foreign 行的组合包只写 patch。
#[tauri::command]
async fn set_plugin_enabled_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    package_name: String,
    enabled: bool,
) -> Result<Vec<String>, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    tauri::async_runtime::spawn_blocking(move || {
        let profile_dir = profiles_dir.join(&profile);
        if !profile_dir.is_dir() {
            return Err(format!("profile 目录不存在: {}", profile_dir.display()));
        }
        let patch_path = profile_dir.join("cordis.patch.yml");
        let service_map = load_service_map(&env, &profile);
        let entries = service_entries_for_plugin(&profile_dir, &service_map, &package_name);

        // 1) patch 行层（market `rowBlock` 格式）
        let mut content = std::fs::read_to_string(&patch_path).unwrap_or_default();
        for (sid, mod_name) in &entries {
            content =
                patchfile::set_plugin_disabled_for(&content, sid, mod_name.as_deref(), !enabled)?;
        }
        let orig = std::fs::read_to_string(&patch_path).unwrap_or_default();
        if content != orig {
            std::fs::write(&patch_path, content).map_err(|e| format!("写入 patch 失败: {e}"))?;
        }

        // 2) bundles 层（market `addProfileBundle`/`removeProfileBundle`）
        //    inbox bundle 永不改动；关闭保留依赖、只动 dsh.profile.bundles
        const INBOX: [&str; 3] = ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app", "@deepseek-ai/dsh-headless"];
        if !INBOX.contains(&package_name.as_str()) && declares_bundle(&profile_dir, &package_name) {
            let pj_path = profile_dir.join("package.json");
            let pj_raw = std::fs::read_to_string(&pj_path).unwrap_or_default();
            if let Ok(mut pj) = serde_json::from_str::<serde_json::Value>(&pj_raw) {
                let bundles = pj
                    .get_mut("dsh")
                    .and_then(|d| d.get_mut("profile"))
                    .and_then(|p| p.get_mut("bundles"))
                    .and_then(|b| b.as_array_mut());
                if let Some(bundles) = bundles {
                    let name = package_name.as_str();
                    let has = bundles.iter().any(|v| v.as_str() == Some(name));
                    if enabled && !has {
                        bundles.push(serde_json::Value::String(name.to_string()));
                    } else if !enabled && has {
                        bundles.retain(|v| v.as_str() != Some(name));
                    }
                    if enabled != has {
                        let out = serde_json::to_string_pretty(&pj)
                            .map_err(|e| format!("序列化 package.json 失败: {e}"))?;
                        std::fs::write(&pj_path, out + "\n")
                            .map_err(|e| format!("写入 package.json 失败: {e}"))?;
                    }
                }
            }
        }

        // 返回新的禁用列表
        let content = std::fs::read_to_string(&patch_path).unwrap_or_default();
        Ok(patchfile::list_disabled_ids(&content))
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 检查插件更新（并行查询 npm latest）
#[tauri::command]
async fn check_updates_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<Vec<PluginUpdate>, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let profile_dir = profiles_dir.join(&profile);
    let pkg = scanner::read_profile_package_full(&profile_dir)
        .ok_or_else(|| "无法读取 profile 的 package.json".to_string())?;
    let names: Vec<String> = pkg.dependencies.keys().cloned().collect();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    // 并行查询 npm dist-tag latest
    let handles: Vec<_> = names
        .iter()
        .map(|name| {
            let name = name.clone();
            std::thread::spawn(move || {
                let registry = settings::load_settings().npm_registry;
                let latest = npm::npm_latest_version(&name, &registry);
                (name, latest)
            })
        })
        .collect();
    let mut updates = Vec::new();
    for h in handles {
        let (name, latest) = h.join().unwrap_or_else(|_| (String::new(), None));
        let current = pkg.dependencies.get(&name).cloned().unwrap_or_default();
        let updatable = match &latest {
            Some(l) => !current.contains(l) && *l != current,
            None => false,
        };
        updates.push(PluginUpdate {
            name,
            current,
            latest: latest.clone().unwrap_or_default(),
            updatable,
            error: if latest.is_none() { Some("查询失败".to_string()) } else { None },
        });
    }
    updates.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(updates)
}

// ==================== 命令：启动/停止（多实例） ====================

/// 运行中进程的 key：env_id + 规范化 profiles 目录 + profile
/// **必须用解析后的绝对/完整目录**，不能用前端传入的原始字符串（"" 与真实路径会键不一致导致停不掉）。
fn run_key(env_id: &str, profiles_dir: &std::path::Path, profile: &str) -> String {
    format!(
        "{}::{}::{}",
        env_id,
        profiles_dir.to_string_lossy(),
        profile
    )
}
fn run_key_legacy(env_id: &str, profile: &str) -> String {
    format!("{env_id}::{profile}")
}

/// 端口选择：用户指定（非 0）→ 校验占用；否则从 3080 起自动分配，
/// 跳过本应用已用端口（used）与系统已占用端口，保证多实例互不冲突。
fn pick_port(requested: Option<u16>, used: &[u16]) -> Result<u16, String> {
    match requested {
        Some(p) if p != 0 => {
            if runner::is_port_open(p) {
                Err(format!("端口 {p} 已被占用，请更换端口或使用自动分配"))
            } else {
                Ok(p)
            }
        }
        _ => (3080..=65535)
            .find(|p| !used.contains(p) && !runner::is_port_open(*p))
            .ok_or_else(|| "无可用端口（3080-65535 全部被占用）".to_string()),
    }
}

/// 内置模板名且目录尚不存在 → 放行（dsh 首次运行自举创建并初始化）
fn builtin_profile_allowed(profile: &str, profiles_dir: &std::path::Path) -> bool {
    let builtin = ["web", "headless", "acp", "sdk", "sdk-minimal"];
    builtin.contains(&profile) && !profiles_dir.join(profile).is_dir()
}

/// 启动前补齐 profile 关键文件：pnpm-workspace.yaml（dsh 模块解析依赖它；
/// 旧版创建 / 部分导入的 profile 可能缺失，缺它会导致启动失败）。
/// dsh 启动时自行处理 bundles 依赖，无需预装 node_modules。
fn ensure_profile_deps(profile: &str, profiles_dir: &std::path::Path) -> Result<(), String> {
    let profile_dir = profiles_dir.join(profile);
    if !profile_dir.is_dir() {
        return Err(format!("profile 不存在: {profile}"));
    }
    // 兼容第三方整合包的非法 github: 依赖 key（pnpm 拒绝，自动规范后 pnpm install 才能通过）
    let pj = profile_dir.join("package.json");
    if pj.is_file() {
        if let Ok(text) = std::fs::read_to_string(&pj) {
            if let Ok(mut data) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(deps) = data
                    .get_mut("dependencies")
                    .and_then(|d| d.as_object_mut())
                {
                    packforge::normalize_dependencies(deps);
                    if let Ok(out) = serde_json::to_string_pretty(&data) {
                        let _ = std::fs::write(&pj, out);
                    }
                }
            }
        }
    }
    let ws = profile_dir.join("pnpm-workspace.yaml");
    if !ws.is_file() {
        std::fs::write(
            &ws,
            "packages:
  - .

nodeLinker: hoisted
autoInstallPeers: false
",
        )
        .map_err(|e| format!("写入 pnpm-workspace.yaml 失败: {e}"))?;
    }
    Ok(())
}

/// 打开开发者工具（F12 使用；生产包亦可用）
#[tauri::command]
fn open_devtools_cmd(window: tauri::WebviewWindow) {
    let _ = window.open_devtools();
}

#[tauri::command]
async fn start_dsh_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    port: Option<u16>,
) -> Result<StartResult, String> {
    let env = get_env_inner(&state, &env_id)?;

    // 环境必须是可启动的 dsh 程序（全局 CLI / 手动添加 / 扫描本体的 dsh）
    if env.run_command.trim().is_empty() {
        return Err("该环境未绑定 dsh 运行时，无法启动".to_string());
    }

    // 启动前确保 node/npm/pnpm 可用（电脑未装时自动下载安装，日志流式）。
    // ensure 内部含 blocking 网络调用（拉取 node 版本索引 / 下载），放阻塞线程池执行。
    let registry = settings::load_settings().npm_registry;
    {
        let app2 = app.clone();
        let reg2 = registry.clone();
        tauri::async_runtime::spawn_blocking(move || toolchain::ensure(&app2, &reg2))
            .await
            .map_err(|e| format!("工具链检查失败：{e}"))?
            .map_err(|e| format!("工具链检查失败：{e}（请检查网络或镜像源）"))?;
    }

    // profile 来源目录（任意 dsh × 任意来源 profile 组合：注入 DSH_HOME）
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    // 启动前兜底：profile 缺依赖（新建未装/导入未装）时自动 pnpm install（日志流式）；
    // 内置模板名（web/headless/acp/sdk/sdk-minimal）目录不存在时跳过——
    // 让 dsh 首次运行自行创建并初始化（官方自举行为，等价 npx @deepseek-ai/dsh web）
    // FS 检查/写文件 → 阻塞线程池
    {
        let profile = profile.clone();
        let profiles_dir = profiles_dir.clone();
        let dep_err = tauri::async_runtime::spawn_blocking(move || {
            if builtin_profile_allowed(&profile, &profiles_dir) {
                Ok(())
            } else {
                ensure_profile_deps(&profile, &profiles_dir)
            }
        })
        .await
        .map_err(|e| format!("profile 依赖检查失败：{e}"))?;
        if let Err(e) = dep_err {
            return Err(format!("profile 依赖检查失败：{e}"));
        }
    }
    let key = run_key(&env_id, &profiles_dir, &profile);
    // per-key 启动互斥：并发双击同一组合时，第二次直接拒绝（防双开/抢端口）
    let _start_guard = {
        let mut starting = state.starting.lock().unwrap();
        if !starting.insert(key.clone()) {
            return Err(format!("「{profile}」正在启动中，请稍候"));
        }
        StartLockGuard { key: key.clone(), state_starting: &state.starting }
    };
    // 锁操作包在独立作用域内：guard 在块尾自动释放。
    // （MutexGuard 非 Send，若跨 await 存活会让 command future 无法跨线程发送）
    // pick_port 内部 TCP 探测端口占用 → 快照 used 后放阻塞线程池，锁不跨 await
    let used: Vec<u16> = {
        let running = state.running.lock().unwrap();
        if let Some(existing) = running.get(&key) {
            return Err(format!(
                "「{profile}」在该环境下已在运行（PID {}，端口 {}），请先停止",
                existing.pid, existing.port
            ));
        }
        running.values().map(|r| r.port).collect()
    };
    // 确定端口：用户指定（非 0）→ 校验占用；否则自动分配（避开本应用已用端口 + 系统占用）
    let port = tauri::async_runtime::spawn_blocking(move || pick_port(port, &used))
        .await
        .map_err(|e| format!("端口选择失败：{e}"))??;

    let dsh_home = runner::dsh_home_of(&profiles_dir);
    // start_dsh 内部解析 launcher / spawn 进程 → 阻塞线程池，不占 tokio worker
    let first = {
        let env = env.clone();
        let profile = profile.clone();
        let dsh_home = dsh_home.clone();
        tauri::async_runtime::spawn_blocking(move || {
            runner::start_dsh(&env, &profile, port, Some(&dsh_home))
        })
        .await
        .map_err(|e| format!("启动任务失败：{e}"))?
    };
    let (pid, log_path) = match first {
        Ok(r) => r,
        Err(first_err) => {
            // 启动解析失败，且原因指向 node 不可用（本机 node <24 跑不了新 dsh）：
            // 强制下载最新 Node LTS 后重试一次，而不是直接报错。
            if first_err.contains("可运行该 DSH 的 Node") {
                let app2 = app.clone();
                let reg2 = registry.clone();
                let install_result = tauri::async_runtime::spawn_blocking(move || {
                    toolchain::ensure_force(&app2, &reg2)
                })
                .await
                .map_err(|e| format!("自动安装 Node LTS 失败：{e}"))?
                .map_err(|e| format!("自动安装 Node LTS 失败：{e}"))?;
                let _ = install_result;
                let env2 = env.clone();
                let profile2 = profile.clone();
                let dsh_home2 = dsh_home.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    runner::start_dsh(&env2, &profile2, port, Some(&dsh_home2))
                })
                .await
                .map_err(|e| format!("启动任务失败：{e}"))??
            } else {
                return Err(first_err);
            }
        }
    };

    // 短暂探测：2 秒后进程仍存活才算启动成功。
    // dsh 若启动失败（例如用旧版 dsh 加载新版 profile → ERR_MODULE_NOT_FOUND）会秒退。
    // 睡眠 + tasklist 探测放阻塞线程池，避免占用 tokio worker 线程导致其他命令排队。
    let (alive, tail) = {
        let log_path = log_path.clone();
        tauri::async_runtime::spawn_blocking(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let alive = runner::is_pid_alive(pid);
            let tail = if alive {
                String::new()
            } else {
                let t = runner::read_log_tail(&log_path, 12);
                let _ = std::fs::remove_file(&log_path);
                t
            };
            (alive, tail)
        })
        .await
        .map_err(|e| format!("等待进程探测失败：{e}"))?
    };
    if !alive {
        return Err(if tail.trim().is_empty() {
            format!("dsh 启动后立即退出（PID {pid}），未捕获到日志")
        } else {
            format!("dsh 启动后立即退出（PID {pid}）。日志尾部：\n{tail}")
        });
    }

    // 日志保留（不删除）：dsh web 的 token 在日志中，供「打开界面」时实时提取
    let url = runner::find_auth_url(&log_path).unwrap_or_else(|| format!("http://127.0.0.1:{port}"));
    let proc = RunningProcess {
        env_id: env_id.clone(),
        profiles_dir: profiles_dir.to_string_lossy().to_string(),
        profile: profile.clone(),
        pid,
        port,
        started_at: runner::now_iso(),
        url: url.clone(),
        log_path: log_path.clone(),
    };
    // 重新加锁插入（旧锁已在跨 await 前释放）；插入前二次校验，
    // 防「检查未运行 → 释放锁 → 启动期间另一并发请求已插入」的双开竞态
    {
        let mut running = state.running.lock().unwrap();
        if running.contains_key(&key) {
            let _ = runner::stop_dsh(pid); // 兜底：杀掉本次多余启动的进程
            return Err(format!("「{profile}」已被另一请求启动，本次已停止"));
        }
        running.insert(key.clone(), proc.clone());
    }

    Ok(StartResult {
        success: true,
        pid: Some(pid),
        message: format!(
            "已启动 dsh {}({}) profile: {}，PID: {pid}，端口 {port}",
            env.version, env.name, profile
        ),
        port,
        url,
    })
}

/// 从 running map 中清理已退出的进程（避免 UI 显示虚假"运行中"）
/// 批量探测（单次 tasklist）。仅可在阻塞线程池内调用（内部 spawn 子进程）。
fn prune_dead(running: &mut HashMap<String, RunningProcess>) {
    if running.is_empty() {
        return;
    }
    let pids: Vec<u32> = running.values().map(|p| p.pid).collect();
    let alive = runner::alive_pids(&pids);
    let dead: Vec<String> = running
        .iter()
        .filter(|(_, p)| !alive.contains(&p.pid))
        .map(|(k, _)| k.clone())
        .collect();
    for k in dead {
        running.remove(&k);
    }
}

/// 清理死进程的异步入口：tasklist 探测放阻塞线程池，锁内只做纯内存删除。
/// 所有高频轮询命令（dsh_status / list_running）必须走这里，绝不能在主线程 spawn 子进程。
async fn prune_dead_async(state: &State<'_, AppState>) {
    // 1) 快照 pid（锁内纯内存）
    let pids: Vec<(String, u32)> = {
        let running = state.running.lock().unwrap();
        running.iter().map(|(k, p)| (k.clone(), p.pid)).collect()
    };
    if pids.is_empty() {
        return;
    }
    // 2) 存活探测（tasklist 子进程）→ 阻塞线程池
    let pid_list: Vec<u32> = pids.iter().map(|(_, pid)| *pid).collect();
    let alive = tauri::async_runtime::spawn_blocking(move || runner::alive_pids(&pid_list))
        .await
        .unwrap_or_default();
    // 3) 删除死进程（锁内纯内存）
    let mut running = state.running.lock().unwrap();
    let dead: Vec<String> = pids
        .into_iter()
        .filter(|(_, pid)| !alive.contains(pid))
        .map(|(k, _)| k)
        .collect();
    for k in dead {
        running.remove(&k);
    }
}

/// 停止指定 env×profile 的进程（每个停止按钮只结束特定进程）
/// 用**解析后的 profiles 目录**做 key；键找不到时再按 env+profile 宽松匹配（避免目录字符串不一致停不掉）。
#[tauri::command]
async fn stop_dsh_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<(), String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let key = run_key(&env_id, &profiles_dir, &profile);
    // 快照取出目标进程（锁不跨 await；MutexGuard 非 Send）
    let mut proc = {
        let mut running = state.running.lock().unwrap();
        let mut proc = running.remove(&key);
        if proc.is_none() {
            // 宽松：同一 env×profile 只有一个实例时直接停它
            let alt = running
                .iter()
                .find(|(k, p)| {
                    p.env_id == env_id && p.profile == profile && k.ends_with(&format!("::{profile}"))
                })
                .map(|(k, _)| k.clone());
            if let Some(k) = alt {
                proc = running.remove(&k);
            }
        }
        proc
    };
    if let Some(p) = proc.take() {
        // tasklist / taskkill 都是子进程 → 阻塞线程池
        let pid = p.pid;
        let stop_result = tauri::async_runtime::spawn_blocking(move || {
            if !runner::is_pid_alive(pid) {
                // 进程已自行退出，视为停止成功
                return Ok(());
            }
            runner::stop_dsh(pid)
        })
        .await
        .map_err(|e| format!("停止任务失败：{e}"))?;
        match stop_result {
            Ok(_) => Ok(()),
            Err(e) => {
                // taskkill 失败但进程可能已退出：把记录放回去
                state.running.lock().unwrap().insert(key, p);
                Err(e)
            }
        }
    } else {
        // 未在运行视为已停止
        Ok(())
    }
}

/// 停止全部运行中的 DSH 进程（关闭软件前调用）。返回停止失败项（空 = 全部成功）。
#[tauri::command]
async fn stop_all_dsh_cmd(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    // 快照运行 key（锁不跨 await；MutexGuard 非 Send）
    // tasklist 清理死进程 → 阻塞线程池（prune_dead 内部 spawn 子进程）
    prune_dead_async(&state).await;
    let keys: Vec<String> = {
        let running = state.running.lock().unwrap();
        running.keys().cloned().collect()
    };
    let mut failed: Vec<String> = Vec::new();
    for key in keys {
        let parts: Vec<&str> = key.splitn(3, "::").collect();
        if parts.len() < 3 {
            failed.push(format!("{key}: 运行记录格式异常"));
            continue;
        }
        let k = key.clone();
        // 锁只在该语句内临时持有
        let proc = {
            let mut running = state.running.lock().unwrap();
            running.remove(&k)
        };
        if let Some(p) = proc {
            // tasklist / taskkill 子进程 → 阻塞线程池
            let r = tauri::async_runtime::spawn_blocking(move || {
                if runner::is_pid_alive(p.pid) {
                    runner::stop_dsh(p.pid).map(|_| p.profile)
                } else {
                    Ok(p.profile)
                }
            })
            .await
            .map_err(|e| format!("{key}: 停止任务失败：{e}"));
            match r {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => failed.push(e),
                Err(e) => failed.push(e),
            }
        }
    }
    Ok(failed)
}

/// 就绪判定（可测试）：端口通 **且**（拿到带 token 的 web URL **或** web 已在 HTTP 服务）。
/// token URL 是最稳的打开方式；但 dsh 的 token 行要等 MCP 初始化完才打印（常 2-3 分钟），
/// 而 web 服务本身早就起来了——浏览器若已有签名 cookie，裸地址即可直接用。
/// 因此 HTTP 已响应（含 401）也算就绪，不再干等 token 行。
fn compute_web_ready(port_open: bool, url: Option<&str>, web_serving: bool) -> bool {
    let has_token = url.map(|u| u.contains("token=")).unwrap_or(false);
    port_open && (has_token || web_serving)
}

/// 查询指定 env×profile 的运行状态（自动清理已退出进程）
/// 轮询频繁（启动就绪轮询 1s/次）：**不做 scan_envs**；tasklist/TCP 探测
/// 一律放阻塞线程池，绝不能在主线程 spawn 子进程（同步 command 跑主线程）。
#[tauri::command]
async fn dsh_status(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<DshStatus, String> {
    prune_dead_async(&state).await;
    let key = run_key(
        &env_id,
        std::path::Path::new(profiles_dir_str.trim()),
        &profile,
    );
    let proc = {
        let running = state.running.lock().unwrap();
        running.get(&key).cloned().or_else(|| {
            running
                .values()
                .find(|p| p.env_id == env_id && p.profile == profile)
                .cloned()
        })
    };
    Ok(match proc {
        Some(p) => {
            let port = p.port;
            let log_path = p.log_path.clone();
            // TCP 探测 + HTTP 探测 + 日志取 token URL → 阻塞线程池（不占主线程）
            let (port_open, auth_url, web_serving) = tauri::async_runtime::spawn_blocking(move || {
                let po = runner::is_port_open(port);
                // token URL：日志出现 `dsh web: http://127.0.0.1:<port>/?token=...`（MCP 初始化完才打印）
                let url = if log_path.is_empty() {
                    None
                } else {
                    runner::find_auth_url(&log_path)
                };
                // HTTP 已响应（含 401）= web 已可访问（有 cookie 的浏览器开裸地址就能用）
                let serving = po && runner::probe_web_serving(port);
                (po, url, serving)
            })
            .await
            .unwrap_or((false, None, false));
            // 未提取到 token URL 时退回裸地址（端口已通时用于兜底展示）
            let url = auth_url.or_else(|| {
                if port_open {
                    Some(format!("http://127.0.0.1:{port}"))
                } else {
                    None
                }
            });
            let web_ready = compute_web_ready(port_open, url.as_deref(), web_serving);
            DshStatus {
                running: true,
                port_open,
                process: Some(p),
                web_ready,
                url,
            }
        }
        None => DshStatus {
            running: false,
            port_open: false,
            process: None,
            web_ready: false,
            url: None,
        },
    })
}

/// 列出全部运行中的 DSH 进程（跨环境跨 profile，自动清理已退出进程）
#[tauri::command]
async fn list_running_cmd(state: State<'_, AppState>) -> Result<Vec<RunningProcess>, String> {
    prune_dead_async(&state).await;
    let running = state.running.lock().unwrap();
    Ok(running.values().cloned().collect())
}

/// 查找目标 env×profile 的孤儿 node 进程（同 bin.js + 同 profile 精确匹配，且不在运行表）。
/// 只查不杀：UI 展示列表，用户确认后调用 kill_orphans_cmd。
#[tauri::command]
async fn find_orphan_nodes_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
) -> Result<Vec<OrphanProcess>, String> {
    let env = get_env_inner(&state, &env_id)?;
    // 快照运行表 PID（排除本会话已知实例）
    let known: Vec<u32> = {
        let running = state.running.lock().unwrap();
        running.values().map(|p| p.pid).collect()
    };
    // WMI 枚举 node 进程 + 命令行匹配 → 阻塞线程池
    tauri::async_runtime::spawn_blocking(move || runner::find_orphans(&env, &profile, &known))
        .await
        .map_err(|e| format!("后台任务失败: {e}"))
}

/// 清理用户确认的孤儿进程（taskkill /T 连 MCP 子树一起收）
#[tauri::command]
async fn kill_orphans_cmd(pids: Vec<u32>) -> Result<Vec<u32>, String> {
    // 返回清理失败的 PID（已退出/不存在视为成功）
    let failed = tauri::async_runtime::spawn_blocking(move || {
        let mut failed = Vec::new();
        for pid in pids {
            if runner::is_pid_alive(pid) {
                if let Err(e) = runner::stop_dsh(pid) {
                    eprintln!("清理孤儿进程 {pid} 失败: {e}");
                    failed.push(pid);
                }
            }
        }
        failed
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?;
    Ok(failed)
}

// ==================== 命令：Profile 导出/导入 ====================

/// 新建 profile（目标目录必须是 profiles 文件夹，不满足报错）
/// 目录创建/写文件放阻塞线程池，不占主线程。
#[tauri::command]
async fn create_profile_cmd(
    state: State<'_, AppState>,
    env_id: String,
    name: String,
    profiles_dir_str: String,
) -> Result<profile_io::CreateProfileResult, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = if profiles_dir_str.trim().is_empty() {
        profiles_dir_of(&env)
    } else {
        let p = std::path::Path::new(profiles_dir_str.trim());
        // 仅接受 profiles 目录本体（文件夹名须为 profiles）
        let leaf = p
            .file_name()
            .map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if leaf != "profiles" {
            return Err("目标目录不是 profiles 文件夹（文件夹名须为 profiles），请重新选择".to_string());
        }
        p.to_path_buf()
    };
    // 目录不存在也允许（create_profile 内部 create_dir_all 递归创建 profiles 目录）
    tauri::async_runtime::spawn_blocking(move || profile_io::create_profile(&profiles_dir, &name))
        .await
        .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 删除 profile（running 时拒绝）
#[tauri::command]
async fn delete_profile_cmd(
    state: State<'_, AppState>,
    env_id: String,
    name: String,
    profiles_dir_str: String,
) -> Result<(), String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    if !profiles_dir.is_dir() {
        return Err(format!("profiles 目录不存在: {}", profiles_dir.display()));
    }
    // 检查该 profile 是否正在运行
    let running = state
        .running
        .lock()
        .unwrap()
        .values()
        .any(|p| p.profile == name);
    let pd = profiles_dir.clone();
    let nm = name.clone();
    tauri::async_runtime::spawn_blocking(move || profile_io::delete_profile(&pd, &nm, running))
        .await
        .map_err(|e| format!("删除任务失败：{e}"))?
}

// ==================== 命令：M5 设置 / DSH 多版本下载 ====================

/// 读取设置
#[tauri::command]
async fn get_settings_cmd() -> settings::Settings {
    tauri::async_runtime::spawn_blocking(settings::load_settings)
        .await
        .unwrap_or_default()
}

/// 保存设置（FS 写 → 阻塞线程池）
#[tauri::command]
async fn set_settings_cmd(
    npm_registry: String,
    dsh_download_dir: String,
    github_mirror: String,
    github_mirrors: Option<Vec<String>>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        settings::validate_registry(&npm_registry)?;
        settings::validate_github_mirror(&github_mirror)?;
        if dsh_download_dir.trim().is_empty() {
            return Err("DSH 下载目录不能为空".to_string());
        }
        let mut s = settings::load_settings();
        s.npm_registry = settings::normalize_registry(&npm_registry);
        s.dsh_download_dir = dsh_download_dir.trim().to_string();
        s.github_mirror = settings::normalize_github_mirror(&github_mirror);
        s.github_mirrors = settings::normalize_github_mirrors(&github_mirrors.unwrap_or_default())?;
        settings::save_settings(&s)
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 项目仓库信息（关于 / 检查更新用）
const PROJECT_REPO: &str = "Liaoyuanxinghuo/DSH-Plugin-Manager";

/// 检测便携运行时是否已就绪（%AppData%\dsh-plugin-manager\runtime\ 内 Node≥24 + npm + pnpm）
/// 内部会逐个探测 node 版本（spawn 子进程）→ 阻塞线程池，不占主线程。
#[tauri::command]
async fn check_portable_runtime_cmd() -> bool {
    tauri::async_runtime::spawn_blocking(|| toolchain::runtime_toolchain().is_some())
        .await
        .unwrap_or(false)
}

/// 初始化便携运行时：完整下载 Node LTS 到 runtime 并安装 pnpm（已就绪则直接返回）。
/// 不改系统 PATH；子进程一律用绝对路径。
#[tauri::command]
async fn init_portable_runtime_cmd(app: tauri::AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(t) = toolchain::runtime_toolchain() {
            return Ok(format!("便携运行时已就绪：{}", t.node.display()));
        }
        let registry = settings::load_settings().npm_registry;
        let tc = toolchain::ensure_force(&app, &registry)?;
        Ok(format!(
            "便携运行时安装完成：node {}（{}）· pnpm {}",
            tc.node.display(),
            tc.node_dir.display(),
            tc.pnpm
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "—".into())
        ))
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 版本号数值化（v 前缀与 rc 后缀忽略，取前 3 段数字）
fn ver_nums(v: &str) -> Vec<u32> {
    v.trim_start_matches('v')
        .split(['.', '-'])
        .filter_map(|s| s.parse::<u32>().ok())
        .take(3)
        .collect()
}

/// 版本比较：a > b 返回 1，a < b 返回 -1，相等 0
fn compare_versions(a: &str, b: &str) -> i32 {
    let x = ver_nums(a);
    let y = ver_nums(b);
    for i in 0..3 {
        let l = x.get(i).copied().unwrap_or(0);
        let r = y.get(i).copied().unwrap_or(0);
        if l != r {
            return if l > r { 1 } else { -1 };
        }
    }
    0
}

/// 走 GitHub 镜像拉取文本（raw 文件 / API），返回 (HTTP 状态码, 响应文本)。
/// 单请求最长 8s，且不超过总截止时间。
fn github_get_text(fetch_url: &str) -> (u16, String) {
    github_get_text_deadline(fetch_url, std::time::Instant::now() + std::time::Duration::from_secs(8))
}

/// 带总截止时间的 GET；剩余时间不足则直接放弃（视为超时）。
fn github_get_text_deadline(fetch_url: &str, deadline: std::time::Instant) -> (u16, String) {
    use std::time::{Duration, Instant};
    let now = Instant::now();
    if now >= deadline {
        return (0, "已达检查更新总超时（30s）".to_string());
    }
    let timeout = (deadline - now).min(Duration::from_secs(8));
    match reqwest::blocking::Client::new()
        .get(fetch_url)
        .timeout(timeout)
        .send()
    {
        Ok(r) => {
            let status = r.status().as_u16();
            let body = r.text().unwrap_or_default();
            (status, body)
        }
        Err(e) => (0, format!("请求失败: {e}")),
    }
}

/// 检查更新（统一走 ghnet 镜像轮询，无总时限；每镜像约 10s 自动切换）：
/// 1) 仓库根 VERSION（main / master）；2) GitHub API releases/latest。
#[tauri::command]
async fn check_update_cmd() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let current = env!("CARGO_PKG_VERSION").to_string();
        let repo_url = format!("https://github.com/{PROJECT_REPO}");
        let mirror = settings::load_settings().github_mirror;

        let mut latest = String::new();
        let mut err_hint = String::new();

        // 1) VERSION 文件（镜像轮询）
        for branch in ["main", "master"] {
            let raw_url = format!("https://raw.githubusercontent.com/{PROJECT_REPO}/{branch}/VERSION");
            match ghnet::fetch_text(&raw_url) {
                Ok((used, body)) => {
                    let v = body.trim().trim_start_matches('v').to_string();
                    if !v.is_empty() && v.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        latest = v;
                        let _ = used;
                        break;
                    }
                }
                Err(e) => err_hint = e,
            }
        }

        // 2) GitHub API（镜像轮询）
        if latest.is_empty() {
            let api_url = format!("https://api.github.com/repos/{PROJECT_REPO}/releases/latest");
            match ghnet::fetch_text(&api_url) {
                Ok((_, body)) => {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                        if let Some(tag) = json.get("tag_name").and_then(|t| t.as_str()) {
                            latest = tag.trim_start_matches('v').to_string();
                        }
                    }
                    if latest.is_empty() {
                        err_hint = "API 响应中无 tag_name".to_string();
                    }
                }
                Err(e) => {
                    if err_hint.is_empty() {
                        err_hint = e;
                    }
                }
            }
        }

        if latest.is_empty() {
            return Ok(serde_json::json!({
                "current": current,
                "latest": "",
                "hasUpdate": false,
                "url": repo_url,
                "error": format!("网络有问题，检查更新失败：{err_hint}。可更换 GitHub 镜像后重试。"),
            }));
        }

        let has_update = compare_versions(&latest, &current) > 0;
        Ok(serde_json::json!({
            "current": current,
            "latest": latest,
            "hasUpdate": has_update,
            "url": repo_url,
            "error": "",
        }))
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 更新安装包资产候选（文件名/ tag 大小写写法不一，全部试一遍）：
/// 点号写法 DSH.Manager_* 为当前 NSIS 实际产物；空格写法兼容旧 release。
fn update_asset_url_candidates(version: &str) -> Vec<(String, String)> {
    let names = [
        format!("DSH.Manager_{version}_x64-setup.exe"),
        format!("DSH Manager_{version}_x64-setup.exe"),
    ];
    let tags = [format!("V{version}"), format!("v{version}")];
    let mut out = Vec::new();
    for tag in &tags {
        for name in &names {
            let enc = name.replace(' ', "%20");
            out.push((
                name.clone(),
                format!("https://github.com/{PROJECT_REPO}/releases/download/{tag}/{enc}"),
            ));
        }
    }
    out
}

/// 资产名匹配规则：包含 "_{version}_x64-setup" 且以 .exe 结尾（大小写不敏感）
/// （DSH.Manager_* / DSH Manager_* 均可命中；版本号中的点不能被替换掉）
fn asset_matches(name: &str, version: &str) -> bool {
    let n = name.to_lowercase();
    n.contains(&format!("_{version}_x64-setup").to_lowercase()) && n.ends_with(".exe")
}

/// 从 GitHub API 的 release assets 里模糊匹配安装包（统一 ghnet 镜像轮询）
fn find_setup_asset(version: &str) -> Result<(String, String), String> {
    let needle = format!("_{version}_x64-setup");
    let mirror = settings::load_settings().github_mirror;
    let urls = [
        format!("https://api.github.com/repos/{PROJECT_REPO}/releases/latest"),
        format!("https://api.github.com/repos/{PROJECT_REPO}/releases/tags/V{version}"),
        format!("https://api.github.com/repos/{PROJECT_REPO}/releases/tags/v{version}"),
    ];
    let mut last_err = String::new();
    for api_url in urls {
        match ghnet::fetch_text(&api_url) {
            Ok((_, body)) => {
                let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) else {
                    continue;
                };
                if let Some(assets) = json.get("assets").and_then(|a| a.as_array()) {
                    for a in assets {
                        let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let url = a
                            .get("browser_download_url")
                            .and_then(|u| u.as_str())
                            .unwrap_or("");
                        if asset_matches(name, version) && !url.is_empty() {
                            return Ok((name.to_string(), url.to_string()));
                        }
                    }
                }
            }
            Err(e) => last_err = e,
        }
    }
    if last_err.is_empty() {
        Err(format!(
            "release 中未找到包含「{needle}」的安装包资产（请确认已上传安装包）"
        ))
    } else {
        Err(format!("获取 release 资产失败：{last_err}"))
    }
}

/// 同一资产的多下载通道（大陆无 VPN）：用户镜像 + 常见代理 + 直连，逐个短超时尝试。
fn expand_download_urls(direct_url: &str, mirror: &str) -> Vec<String> {
    let mut proxies: Vec<String> = vec![];
    let m = mirror.trim().trim_end_matches('/').to_string();
    if !m.is_empty() {
        proxies.push(m);
    }
    for p in [
        "https://ghfast.top",
        "https://gh-proxy.com",
        "https://ghproxy.net",
        "https://ghp.ci",
    ] {
        if !proxies.iter().any(|x| x == p) {
            proxies.push(p.to_string());
        }
    }
    let mut out: Vec<String> = proxies
        .iter()
        .map(|p| format!("{p}/{direct_url}"))
        .collect();
    out.push(direct_url.to_string());
    out
}

/// 下载新版本安装包（走 GitHub 镜像，大陆无 VPN 可用）：
/// release 资产命名固定：DSH Manager_{version}_x64-setup.exe
/// 保存到 %USERPROFILE%\Downloads，下载完成后自动启动安装程序。
#[tauri::command]
async fn download_update_cmd(version: String, app: tauri::AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {

    use std::io::{Read, Write};
    use tauri::Emitter;

    let version = version.trim_start_matches('v').to_string();
    if version.is_empty() || !version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Err("无效的版本号".to_string());
    }
    let mirror = settings::load_settings().github_mirror;
    // 优先按 release 资产列表模糊匹配（资产名包含 _{v}_x64-setup 即可）；
    // 拿不到资产列表时回退固定命名构造 URL
    // 优先 API 资产真实 URL；失败则按 tag 大小写 / 文件名点号或空格多候选回退
    let candidates: Vec<(String, String)> = match find_setup_asset(&version) {
        Ok((n, u)) => vec![(n, u)],
        Err(_) => update_asset_url_candidates(&version),
    };
    let download_dir = std::env::var("USERPROFILE")
        .map(|u| std::path::Path::new(&u).join("Downloads"))
        .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join("Downloads")))
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    std::fs::create_dir_all(&download_dir).map_err(|e| format!("无法创建下载目录: {e}"))?;

    // 统一 ghnet 镜像轮询下载（用户镜像优先，失败自动切内置镜像，最后直连）
    let mut last_err = String::new();
    let mut fname = String::new();
    let mut fetched: Option<reqwest::blocking::Response> = None;
    for (name, direct_url) in candidates {
        match ghnet::send_get(&direct_url) {
            Ok((_used_url, resp)) => {
                fname = name;
                fetched = Some(resp);
                break;
            }
            Err(e) => last_err = e,
        }
    }
    let Some(mut resp) = fetched else {
        return Err(format!(
            "网络有问题，下载失败：{last_err}。可手动下载安装包，或在设置中更换 GitHub 镜像后重试"
        ));
    };
    let out_path = download_dir.join(&fname);

    let total = resp.content_length().unwrap_or(0);

    let emit_progress = |done: u64| {
        let percent = if total > 0 {
            ((done as f64 / total as f64) * 100.0) as u32
        } else {
            0
        };
        let _ = app.emit(
            "update-download",
            serde_json::json!({ "done": done, "total": total, "percent": percent }),
        );
    };

    let mut out_file = std::fs::File::create(&out_path).map_err(|e| format!("创建文件失败: {e}"))?;
    let mut buf = [0u8; 65536];
    let mut done: u64 = 0;
    loop {
        let n = resp
            .read(&mut buf)
            .map_err(|e| format!("下载中断：{e}"))?;
        if n == 0 {
            break;
        }
        out_file
            .write_all(&buf[..n])
            .map_err(|e| format!("写入文件失败: {e}"))?;
        done += n as u64;
        if done % (1024 * 1024) < 65536 {
            emit_progress(done);
        }
    }
    emit_progress(done);
    let out_str = out_path.to_string_lossy().to_string();
    let _ = app.emit(
        "update-download",
        serde_json::json!({ "done": true, "path": out_str }),
    );
    Ok(out_str)

    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 启动已下载的安装程序，然后立即退出当前程序（让安装器能覆盖正在运行的 exe）。
/// 安装器为独立进程，父进程退出不影响它继续运行。
#[tauri::command]
async fn launch_installer_and_exit_cmd(path: String, app: tauri::AppHandle) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("安装程序路径为空".to_string());
    }
    if !std::path::Path::new(&path).exists() {
        return Err(format!("安装程序不存在: {path}"));
    }
    // spawn + 等待放阻塞线程池，勿在主线程 sleep
    tauri::async_runtime::spawn_blocking(move || {
        std::process::Command::new(&path)
            .spawn()
            .map_err(|e| format!("启动安装程序失败：{e}"))?;
        // 稍等片刻确保安装程序已拉起，再退出自己
        std::thread::sleep(std::time::Duration::from_millis(1200));
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))??;
    app.exit(0);
    Ok(())
}

/// 列出 DSH 全部版本
#[tauri::command]
async fn list_dsh_versions_cmd() -> Result<Vec<dsh_install::DshVersionInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || {

    let registry = settings::load_settings().npm_registry;
    dsh_install::list_dsh_versions(&registry)

    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 下载并安装指定 DSH 版本到目标目录（流式日志）
#[tauri::command]
async fn install_dsh_version_cmd(
    app: tauri::AppHandle,
    version: String,
    target_dir: String,
) -> Result<dsh_install::DshInstallResult, String> {
    // 下载+安装较耗时且含 blocking 网络调用（工具链 ensure / npm install），
    // 整体放到阻塞线程池执行，避免卡 UI / tokio blocking panic。
    tauri::async_runtime::spawn_blocking(move || {
        if version.trim().is_empty() {
            return Err("版本不能为空".to_string());
        }
        if target_dir.trim().is_empty() {
            return Err("目标目录不能为空".to_string());
        }
        let registry = settings::load_settings().npm_registry;
        Ok(dsh_install::install_dsh_version(
            &app,
            version.trim(),
            target_dir.trim(),
            &registry,
        ))
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 确保 profile 的 .npmrc 使用当前镜像（安装插件前调用）
#[tauri::command]
async fn ensure_profile_npmrc_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<(), String> {
    let env = get_env_inner(&state, &env_id)?;
    let profile_dir = profiles_dir_arg(&profiles_dir_str, &env).join(&profile);
    tauri::async_runtime::spawn_blocking(move || {
        if !profile_dir.is_dir() {
            return Err(format!("profile 不存在: {profile}"));
        }
        let registry = settings::load_settings().npm_registry;
        dsh_install::ensure_profile_npmrc(&profile_dir, &registry)
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

// ==================== 命令：M4 依赖健康 / 残留清理 / 诊断导出 ====================

/// 依赖健康检查（FS 扫描 → 阻塞线程池）
#[tauri::command]
async fn check_deps_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<Vec<health::DepIssue>, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profile_dir = profiles_dir_arg(&profiles_dir_str, &env).join(&profile);
    tauri::async_runtime::spawn_blocking(move || health::check_profile_deps(&profile_dir))
        .await
        .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 修复依赖：在 profile 目录执行 dsh plugin install（pnpm install，流式日志）
/// 必须先 ensure 便携工具链（run_dsh_with_logs 内）并写入 **用户设置的 npm 镜像**（.npmrc）
#[tauri::command]
async fn fix_deps_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<installer::InstallOutcome, String> {
    let env = get_env_inner(&state, &env_id)?;
    if env.run_command.trim().is_empty() {
        return Err("该环境未绑定 dsh 运行时，无法修复依赖".to_string());
    }
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let profile_dir = profiles_dir.join(&profile);
    // 修复前先规范 package.json 依赖声明（兼容 github: 前缀 key），否则 pnpm 直接失败
    let _ = ensure_profile_deps(&profile, &profiles_dir);
    // npm 源必须用用户设置的镜像（默认 npmmirror），否则大陆拉包失败
    let registry = settings::load_settings().npm_registry;
    let _ = dsh_install::ensure_profile_npmrc(&profile_dir, &registry);
    let dsh_home = runner::dsh_home_of(&profiles_dir);
    let app2 = app.clone();
    let env2 = env.clone();
    let args = ["plugin".to_string(), "--profile".to_string(), profile, "install".to_string()];
    // pnpm install 子进程较久，放阻塞线程池
    tauri::async_runtime::spawn_blocking(move || {
        Ok(installer::run_dsh_with_logs(&app2, &env2, &args, Some(&dsh_home)))
    })
    .await
    .map_err(|e| format!("后台任务失败：{e}"))?
}

/// 扫描 profile 内残留/缓存目录
#[tauri::command]
async fn scan_junk_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<Vec<health::JunkEntry>, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profile_dir = profiles_dir_arg(&profiles_dir_str, &env).join(&profile);
    if !profile_dir.is_dir() {
        return Err(format!("profile 不存在: {profile}"));
    }
    Ok(health::scan_junk(&profile_dir))
}

/// 清理残留目录
#[tauri::command]
async fn clean_junk_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    names: Vec<String>,
) -> Result<Vec<String>, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profile_dir = profiles_dir_arg(&profiles_dir_str, &env).join(&profile);
    if !profile_dir.is_dir() {
        return Err(format!("profile 不存在: {profile}"));
    }
    let pd = profile_dir.clone();
    let ns = names.clone();
    tauri::async_runtime::spawn_blocking(move || health::clean_junk(&pd, &ns))
        .await
        .map_err(|e| format!("清理任务失败：{e}"))?
}

/// 导出诊断包（配置文件 + 会话/存储摘要 + 各 profile 元数据）
#[tauri::command]
async fn export_diag_cmd(
    state: State<'_, AppState>,
    env_id: String,
    target_path: String,
) -> Result<profile_io::ExportResult, String> {
    let env = get_env_inner(&state, &env_id)?;
    let home = std::path::Path::new(&env.home_dir);
    let target = if target_path.trim().is_empty() {
        let backups = home.join("backups");
        let _ = std::fs::create_dir_all(&backups);
        backups.join(format!("dsh-diag-{}.zip", timestamp_compact()))
    } else {
        std::path::PathBuf::from(target_path)
    };
    profile_io::export_diag(home, &target)
}

/// 计算环境 profiles 目录（扫描目录适配）
/// 解析 profiles 目录：优先命令传入（任意 dsh × 任意来源 profile 组合），否则退回环境默认
fn profiles_dir_arg(profiles_dir_str: &str, env: &DshEnv) -> std::path::PathBuf {
    if !profiles_dir_str.trim().is_empty() {
        std::path::PathBuf::from(profiles_dir_str.trim())
    } else {
        profiles_dir_of(env)
    }
}

fn profiles_dir_of(env: &DshEnv) -> std::path::PathBuf {
    env.scan_profiles_dir
        .clone()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(&env.home_dir).join("profiles"))
}

/// 导出 profile 为 zip 压缩包
#[tauri::command]
async fn export_profile_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    target_path: String,
    exclude_node_modules: Option<bool>,
) -> Result<profile_io::ExportResult, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let profile_dir = profiles_dir.join(&profile);
    if !profile_dir.is_dir() {
        return Err(format!("profile 不存在: {profile}"));
    }
    // 目标路径缺省：~/.dsh/backups/<profile>-<时间戳>.zip
    let target = if target_path.trim().is_empty() {
        let backups = std::path::Path::new(&env.home_dir).join("backups");
        let _ = std::fs::create_dir_all(&backups);
        backups
            .join(format!("{profile}-{}.zip", timestamp_compact()))
    } else {
        std::path::PathBuf::from(target_path)
    };
    if let Some(dir) = target.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let excl = exclude_node_modules.unwrap_or(false);
    // zip 打包放阻塞线程池
    tauri::async_runtime::spawn_blocking(move || profile_io::export_profile(&profile_dir, &target, excl))
        .await
        .map_err(|e| format!("导出任务失败：{e}"))?
}

/// 复制 profile 到同目录副本，自动补 -N 递增名（N 从 1 起，跳过已存在），
/// 保留 node_modules（复制后立即可用，无需重建依赖）。
#[tauri::command]
async fn clone_profile_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<serde_json::Value, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let src = profiles_dir.join(&profile);
    if !src.is_dir() {
        return Err(format!("profile 不存在: {profile}"));
    }
    // 运行中的 profile 有文件被进程锁定（日志/session），复制会遇「拒绝访问」——
    // 明确提示先停止，避免复制出残缺副本
    let running_this = state
        .running
        .lock()
        .unwrap()
        .values()
        .any(|p| p.env_id == env_id && p.profile == profile && p.profiles_dir == profiles_dir);
    if running_this {
        return Err(format!("「{profile}」正在运行，请先停止相关实例再复制副本"));
    }
    let mut n: u32 = 1;
    let mut dest = profiles_dir.join(format!("{profile}-{n}"));
    while dest.exists() {
        n += 1;
        dest = profiles_dir.join(format!("{profile}-{n}"));
    }
    let final_name = dest
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let src2 = src.clone();
    let dest2 = dest.clone();
    // 大目录（node_modules）复制放阻塞线程池；
    // 权限/占用错误跳过并计数（多为运行期临时文件、只读文件），不中断整体复制
    let (files, skipped) = tauri::async_runtime::spawn_blocking(move || copy_dir_all(&src2, &dest2))
        .await
        .map_err(|e| format!("复制任务失败：{e}"))?
        .map_err(|e| format!("复制失败：{e}"))?;
    if files == 0 && skipped == 0 {
        return Err("复制失败：目录为空或不可读".to_string());
    }
    // 修改副本 package.json 的 name 字段为 dsh-profile-{新名}，避免与原件同名
    let pj = dest.join("package.json");
    if pj.is_file() {
        if let Ok(text) = std::fs::read_to_string(&pj) {
            if let Ok(mut data) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(obj) = data.as_object_mut() {
                    obj.insert(
                        "name".to_string(),
                        serde_json::Value::String(format!("dsh-profile-{final_name}")),
                    );
                    if let Ok(out) = serde_json::to_string_pretty(&data) {
                        let _ = std::fs::write(&pj, out);
                    }
                }
            }
        }
    }
    Ok(serde_json::json!({ "name": final_name, "skipped": skipped, "files": files }))
}

/// 递归复制目录树（含子目录与隐藏文件）。
/// 返回 (已复制文件数, 跳过的文件数)；权限拒绝/文件被占用（os error 5 等）跳过计数，
/// 其余错误返回 Err。符号链接（junction）跳过避免递归进不可达目标。
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<(usize, usize)> {
    let mut files = 0usize;
    let mut skipped = 0usize;
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            let (f, s) = copy_dir_all(&from, &to)?;
            files += f;
            skipped += s;
        } else if ty.is_symlink() {
            skipped += 1; // junction / 符号链接：不跟随，避免不可达目标
        } else {
            match std::fs::copy(&from, &to) {
                Ok(_) => files += 1,
                Err(e)
                    if e.kind() == std::io::ErrorKind::PermissionDenied
                        || e.raw_os_error() == Some(5) =>
                {
                    skipped += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }
    Ok((files, skipped))
}

/// 导入 zip 到指定环境的 profiles 目录（重名自动改名）
#[tauri::command]
async fn import_profile_cmd(
    state: State<'_, AppState>,
    env_id: String,
    zip_path: String,
    profiles_dir_str: String,
) -> Result<profile_io::ImportResult, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    // 解压放阻塞线程池
    tauri::async_runtime::spawn_blocking(move || {
        profile_io::import_profile(std::path::Path::new(&zip_path), &profiles_dir)
    })
    .await
    .map_err(|e| format!("导入任务失败：{e}"))?
}

/// 获取 profile 的 node_modules 体积（导出前提示用）
#[tauri::command]
async fn profile_node_modules_size(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<u64, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let profile_dir = profiles_dir.join(&profile);
    // node_modules 可能巨大，递归遍历放阻塞线程池
    let p2 = profile_dir.clone();
    tauri::async_runtime::spawn_blocking(move || profile_io::node_modules_size(&p2))
        .await
        .map_err(|e| format!("统计失败：{e}"))
}

// ==================== 命令：整合包（DSH-PackForge） ====================

/// 导出整合包：profile → .dspack v3
/// `dsh_hint`：下拉选择的「适配版本」写入 manifest.dshVersion；空则用当前环境版本
#[tauri::command]
async fn export_pack_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    target_path: String,
    pack_name: String,
    pack_version: String,
    display_name: String,
    dsh_hint: Option<String>,
) -> Result<packforge::PackExportResult, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let profile_dir = profiles_dir.join(&profile);
    if !profile_dir.is_dir() {
        return Err(format!("profile 不存在: {profile}"));
    }
    let name = if pack_name.trim().is_empty() { profile.clone() } else { pack_name };
    let default_file = format!("{}-{}.dspack", name.trim(), if pack_version.trim().is_empty() { "1.0.0" } else { pack_version.trim() });
    let target = if target_path.trim().is_empty() {
        let backups = std::path::Path::new(&env.home_dir).join("backups");
        let _ = std::fs::create_dir_all(&backups);
        backups.join(default_file)
    } else {
        std::path::PathBuf::from(target_path)
    };
    // 适配版本：下拉指定优先，否则当前环境版本
    let hint = dsh_hint
        .unwrap_or_default()
        .trim()
        .trim_start_matches('v')
        .to_string();
    let env_ver = if hint.is_empty() {
        env.version.clone()
    } else {
        hint
    };
    // zip 打包放阻塞线程池
    tauri::async_runtime::spawn_blocking(move || {
        packforge::export_pack(
            &profile_dir,
            &profiles_dir,
            &target,
            &name,
            &pack_version,
            &display_name,
            &env_ver,
        )
    })
    .await
    .map_err(|e| format!("导出任务失败：{e}"))?
}

/// 拉取整合包市场索引（dsh-pack-market index.json）
#[tauri::command]
async fn market_packs_cmd() -> Result<Vec<packforge::MarketPackEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || {

    packforge::read_market_index()

    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 下载整合包到缓存目录并校验 sha256 + size
#[tauri::command]
async fn download_pack_cmd(
    url: String,
    sha256: String,
    size: u64,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {

    let dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("dsh-plugin-manager")
        .join("packs");
    packforge::download_pack(&url, &sha256, size, &dir)

    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 导入整合包（本地 .dspack 文件）到目标 profiles 目录
#[tauri::command]
async fn import_pack_cmd(
    state: State<'_, AppState>,
    env_id: String,
    pack_path: String,
    profiles_dir_str: String,
) -> Result<packforge::PackImportResult, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    // 解压放阻塞线程池
    tauri::async_runtime::spawn_blocking(move || {
        packforge::import_pack(std::path::Path::new(&pack_path), &profiles_dir)
    })
    .await
    .map_err(|e| format!("导入任务失败：{e}"))?
}

// ==================== 命令：profile 备注 ====================

/// 读取全部 profile 备注
#[tauri::command]
async fn get_profile_notes_cmd() -> Result<packforge::NotesMap, String> {
    tauri::async_runtime::spawn_blocking(packforge::load_notes)
        .await
        .map_err(|e| format!("后台任务失败: {e}"))
}

/// 保存 profile 备注（清空则删除；FS 写 → 阻塞线程池）
#[tauri::command]
async fn save_profile_note_cmd(
    profile: String,
    profiles_dir_str: String,
    note: String,
    hint_version: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let key = packforge::note_key(&profile, &profiles_dir_str);
        packforge::save_note(
            &key,
            &packforge::ProfileNote {
                note,
                hint_version,
            },
        )
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

fn timestamp_compact() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}", now)
}

// ==================== 命令：在线插件安装（M2） ====================

/// 搜索 npm 包
#[tauri::command]
async fn npm_search_cmd(query: String) -> Result<Vec<npm::NpmSearchHit>, String> {
    tauri::async_runtime::spawn_blocking(move || {

    let registry = settings::load_settings().npm_registry;
    npm::npm_search(&query, &registry)

    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 获取包全部版本信息
#[tauri::command]
async fn npm_package_info_cmd(name: String) -> Result<npm::NpmPackageInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {

    let registry = settings::load_settings().npm_registry;
    npm::npm_package_info(&name, &registry)

    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 兼容性预检：指定插件版本 vs 运行时版本
#[tauri::command]
async fn check_compat_cmd(
    name: String,
    version: String,
    runtime_version: String,
) -> Result<Vec<npm::PeerIssue>, String> {
    // npm 包信息查询是 blocking 网络调用，放阻塞线程池，避免占 IPC 线程
    tauri::async_runtime::spawn_blocking(move || {
        let registry = settings::load_settings().npm_registry;
        let info = npm::npm_package_info(&name, &registry)?;
        let vinfo = info
            .versions
            .iter()
            .find(|v| v.version == version)
            .ok_or_else(|| format!("未找到版本 {version}"))?;
        Ok(npm::check_peer_compat(&vinfo.peer_dependencies, &runtime_version))
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 拉取插件市场目录
#[tauri::command]
async fn market_catalog_cmd() -> Result<market::MarketCatalog, String> {
    tauri::async_runtime::spawn_blocking(move || {

    market::market_catalog()

    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 修复 pnpm 构建白名单（git 源插件安装需要）
#[tauri::command]
async fn fix_build_permit_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    packages: Vec<String>,
) -> Result<Vec<String>, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let profile_dir = profiles_dir.join(profile);
    tauri::async_runtime::spawn_blocking(move || {
        if !profile_dir.is_dir() {
            return Err(format!("profile 目录不存在: {}", profile_dir.display()));
        }
        buildpermit::fix_build_permit(&profile_dir, &packages)
    })
    .await
    .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 本地安装插件：文件夹（file:）或 .tgz 压缩包
#[tauri::command]
async fn install_local_plugin_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    path: String,
) -> Result<installer::InstallOutcome, String> {
    let env = get_env_inner(&state, &env_id)?;
    if env.run_command.trim().is_empty() {
        return Err("该环境未绑定 dsh 运行时，无法安装插件".to_string());
    }
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("路径不存在: {path}"));
    }
    let spec = local_plugin_spec(&path)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let app2 = app.clone();
    let env2 = env.clone();
    // pnpm 子进程较久，放阻塞线程池
    tauri::async_runtime::spawn_blocking(move || {
        Ok(installer::install_plugin(&app2, &env2, &profile, &spec, &profiles_dir))
    })
    .await
    .map_err(|e| format!("后台任务失败：{e}"))?
}

/// 由本地路径构造安装 spec：
/// - 文件夹 → 裸绝对路径（正斜杠）。实测：pnpm 命令行 `add` 对
///   `file:E:/...`（10.34.5 当相对路径拼 cwd 报 ENOENT）、`file:///E:/...`/`file:/E:/...`
///   （10.34.5 拼到用户目录报 ENOENT）均失败；只有裸绝对路径在 pnpm 10.32.1 与 10.34.5
///   下都成功。dsh 转发层对绝对路径原样透传，安全。
/// - .tgz/.tar.gz → 原路径（pnpm 原生支持）
fn local_plugin_spec(path: &str) -> Result<String, String> {
    let p = std::path::Path::new(path);
    if p.is_dir() {
        // Windows 上 canonicalize 返回带 \\?\ 前缀的扩展路径；
        // 剥掉前缀（否则 pnpm 会规范化为 /?/ 导致目录找不到），并把反斜杠统一为正斜杠。
        let canon = p.canonicalize().map_err(|e| format!("解析路径失败: {e}"))?;
        let canon_str = canon.to_string_lossy();
        let canon_str = canon_str.strip_prefix("\\\\?\\").unwrap_or(canon_str.as_ref());
        Ok(canon_str.replace('\\', "/"))
    } else if path.to_lowercase().ends_with(".tgz") || path.to_lowercase().ends_with(".tar.gz") {
        Ok(path.to_string())
    } else {
        Err("仅支持文件夹或 .tgz/.tar.gz 压缩包".to_string())
    }
}

/// 在线安装插件（流式日志）
#[tauri::command]
async fn install_plugin_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    spec: String,
) -> Result<installer::InstallOutcome, String> {
    let env = get_env_inner(&state, &env_id)?;
    if env.run_command.trim().is_empty() {
        return Err("该环境未绑定 dsh 运行时，无法安装插件".to_string());
    }
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let app2 = app.clone();
    let env2 = env.clone();
    // pnpm 子进程较久，放阻塞线程池
    tauri::async_runtime::spawn_blocking(move || {
        Ok(installer::install_plugin(&app2, &env2, &profile, &spec, &profiles_dir))
    })
    .await
    .map_err(|e| format!("后台任务失败：{e}"))?
}

/// 卸载插件
#[tauri::command]
async fn remove_plugin_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    name: String,
) -> Result<installer::InstallOutcome, String> {
    let env = get_env_inner(&state, &env_id)?;
    if env.run_command.trim().is_empty() {
        return Err("该环境未绑定 dsh 运行时，无法卸载插件".to_string());
    }
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let app2 = app.clone();
    let env2 = env.clone();
    // pnpm 子进程较久，放阻塞线程池
    tauri::async_runtime::spawn_blocking(move || {
        Ok(installer::remove_plugin(&app2, &env2, &profile, &name, &profiles_dir))
    })
    .await
    .map_err(|e| format!("后台任务失败：{e}"))?
}

/// 豁免版本校验（跑 dsh 子进程）→ 阻塞线程池
#[tauri::command]
async fn allow_version_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    pkg_spec: String,
) -> Result<installer::InstallOutcome, String> {
    let env = get_env_inner(&state, &env_id)?;
    if env.run_command.trim().is_empty() {
        return Err("该环境未绑定 dsh 运行时，无法执行豁免".to_string());
    }
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    tauri::async_runtime::spawn_blocking(move || {
        Ok(installer::allow_version(&app, &env, &pkg_spec, &profiles_dir))
    })
    .await
    .map_err(|e| format!("后台任务失败：{e}"))?
}

/// 撤销豁免（跑 dsh 子进程）→ 阻塞线程池
#[tauri::command]
async fn disallow_version_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    pkg_spec: String,
) -> Result<installer::InstallOutcome, String> {
    let env = get_env_inner(&state, &env_id)?;
    if env.run_command.trim().is_empty() {
        return Err("该环境未绑定 dsh 运行时，无法撤销豁免".to_string());
    }
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    tauri::async_runtime::spawn_blocking(move || {
        Ok(installer::disallow_version(&app, &env, &pkg_spec, &profiles_dir))
    })
    .await
    .map_err(|e| format!("后台任务失败：{e}"))?
}

// ==================== 命令：文件/URL ====================

/// 在资源管理器中打开路径（spawn 子进程 → 阻塞线程池）
#[tauri::command]
async fn open_path(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || fsutil::open_in_explorer(&path))
        .await
        .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 用默认浏览器打开 URL（spawn 子进程 → 阻塞线程池）
#[tauri::command]
async fn open_url_cmd(url: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || fsutil::open_url(&url))
        .await
        .map_err(|e| format!("后台任务失败: {e}"))?
}

/// 切换 WebView DevTools（F12 / Ctrl+Shift+I）。
/// 依赖 tauri 的 `devtools` feature（Cargo.toml 已开）：release/安装版也可用。
/// WebView2 的浏览器快捷键在 DevTools 关闭时会吞掉 F12 且不转发给页面，
/// 因此前端监听到 F12 时显式调本命令兜底。
#[tauri::command]
fn toggle_devtools(window: tauri::WebviewWindow) {
    if window.is_devtools_open() {
        window.close_devtools();
    } else {
        window.open_devtools();
    }
}

/// 打开指定 env×profile 的 DSH web 界面。
/// **不调用 get_env_inner/scan_envs**（全盘扫描会卡住 UI）；只查 running 表 + 读日志取 token。
/// **只打开带 token 的认证地址**（裸地址无当前进程 cookie 必 401）。
/// token 行要等 Loader 树结算才打印（多 MCP 的 profile 常要 2-3 分钟）：
/// 已就绪则立刻打开；未就绪则后台等待并自动打开，立刻返回提示不阻塞 UI。
/// `browser_exe`：浏览器下拉框选中的 exe（空串 = 系统默认浏览器），全局记忆由前端持久化。
#[tauri::command]
async fn open_dsh_web_cmd(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    browser_exe: String,
) -> Result<String, String> {
    // 1) 在 running 里定位实例（精确 key 或 env×profile 宽松匹配），锁不跨 await
    let log_path = {
        let running = state.running.lock().unwrap();
        let key = run_key(
            &env_id,
            std::path::Path::new(profiles_dir_str.trim()),
            &profile,
        );
        let proc = running
            .get(&key)
            .cloned()
            .or_else(|| {
                running
                    .values()
                    .find(|p| p.env_id == env_id && p.profile == profile)
                    .cloned()
            })
            .ok_or_else(|| format!("「{profile}」在该环境未在运行"))?;
        proc
    };
    // 2) 读日志放阻塞池。已就绪秒开 token 地址；token 行被插件拖住时用持久密钥
    //    自铸 cookie 跳转进入（不再干等 token 行，也不开裸地址吃 401）。
    tauri::async_runtime::spawn_blocking(move || {
        let log = log_path.log_path.clone();
        // 快路径：token 已在日志 → 立刻打开
        if !log.is_empty() {
            if let Some(url) = runner::find_auth_url(&log) {
                if url.contains("token=") {
                    browsers::open_url_with(&browser_exe, &url)?;
                    return Ok("已在浏览器打开 DSH 界面".to_string());
                }
            }
        }
        let port = log_path.port;
        let profiles_dir = std::path::PathBuf::from(&log_path.profiles_dir);
        // DSH_HOME = profiles_dir 的父目录（与启动注入语义一致）
        let dsh_home = profiles_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or(profiles_dir);
        // 快路径没 token → 免 token 铸 cookie 进入（dsh-mcp-connector 等会拖住 token 行）
        if runner::is_port_open(port) {
            match webauth::open_web_via_cookie_hop(&dsh_home, port, &browser_exe) {
                Ok(_) => {
                    return Ok("已在浏览器打开 DSH 界面（免 token 登录）".to_string());
                }
                Err(e) => {
                    // 密钥读不到再退后台等 token（至少还能等到就开）
                    let log_bg = log.clone();
                    let browser_bg = browser_exe.clone();
                    std::thread::spawn(move || {
                        if let Ok(url) = runner::resolve_web_open_url(&log_bg, 300_000) {
                            let _ = browsers::open_url_with(&browser_bg, &url);
                        }
                    });
                    return Ok(format!(
                        "认证地址尚未生成，且免 token 登录失败（{e}）；已后台等待 token 行，就绪后自动打开"
                    ));
                }
            }
        }
        // 端口未通：后台等 token 行
        let log_bg = log.clone();
        let browser_bg = browser_exe.clone();
        std::thread::spawn(move || {
            if let Ok(url) = runner::resolve_web_open_url(&log_bg, 300_000) {
                let _ = browsers::open_url_with(&browser_bg, &url);
            }
        });
        Ok("服务尚未监听端口，已后台等待认证地址，就绪后将自动打开浏览器".to_string())
    })
    .await
    .map_err(|e| format!("打开浏览器失败: {e}"))?
}

/// 枚举已安装浏览器：第一项系统默认，其后各浏览器（带图标 data URL）。
/// 供「打开界面」旁的浏览器下拉框使用；探测走注册表 + 图标缓存，放阻塞池。
#[tauri::command]
async fn list_browsers_cmd() -> Result<Vec<browsers::BrowserInfo>, String> {
    tauri::async_runtime::spawn_blocking(browsers::list_browsers)
        .await
        .map_err(|e| format!("后台任务失败: {e}"))
}

/// 获取当前选中"环境 × Profile"的关键路径集合（供前端文件按钮使用）。
/// 以选中 profile 的来源目录为基准：sessions/logs 位于其父目录（即注入的 DSH_HOME）。
#[tauri::command]
async fn get_env_paths(
    state: State<'_, AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<serde_json::Value, String> {
    // get_env_inner 兜底会 scan_envs（spawn 探测）→ 先快路径，再阻塞线程池
    let env = {
        let manual = state.manual_envs.lock().unwrap();
        manual.iter().find(|e| e.id == env_id).cloned()
    };
    let env = match env {
        Some(e) => e,
        None => {
            let manual = state.manual_envs.lock().unwrap().clone();
            let scans = state.scan_dirs.lock().unwrap().clone();
            let id = env_id.clone();
            tauri::async_runtime::spawn_blocking(move || {
                scanner::scan_envs(&manual, &scans)
                    .into_iter()
                    .find(|e| e.id == id)
            })
            .await
            .unwrap_or(None)
            .ok_or_else(|| format!("环境不存在: {env_id}"))?
        }
    };
    let home = std::path::Path::new(&env.home_dir);
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    // 当前选中 profile 的目录（存在才返回，不存在返回 null）
    let profile_dir = if profile.is_empty() {
        None
    } else {
        let d = std::path::Path::new(&profiles_dir).join(&profile);
        if d.is_dir() {
            Some(d.to_string_lossy().to_string())
        } else {
            None
        }
    };
    // DSH_HOME = profiles_dir 的父目录（与启动注入语义一致）
    let dsh_home = std::path::Path::new(&profiles_dir)
        .parent()
        .unwrap_or(home);
    Ok(serde_json::json!({
        "homeDir": env.home_dir,
        "dshHome": dsh_home.to_string_lossy().to_string(),
        "profileDir": profile_dir,
        "profilesDir": profiles_dir,
        "sessionsDir": dsh_home.join("sessions").to_string_lossy().to_string(),
        "logsDir": dsh_home.join("logs").to_string_lossy().to_string(),
        "binPath": env.bin_path,
    }))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let manual_envs = load_manual_envs();
    let scan_dirs = load_scan_dirs();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            manual_envs: Mutex::new(manual_envs),
            scan_dirs: Mutex::new(scan_dirs),
            running: Mutex::new(HashMap::new()),
            starting: Mutex::new(std::collections::HashSet::new()),
        })
        .invoke_handler(tauri::generate_handler![
            scan_envs,
            add_manual_env,
            remove_manual_env,
            get_env,
            add_scan_dir,
            remove_scan_dir,
            list_scan_dirs,
            add_dsh_scan_dir,
            list_all_profiles,
            list_profiles_from_dir,
            scan_profiles_live,
            list_profiles,
            create_profile_cmd,
            delete_profile_cmd,
            open_devtools_cmd,
            list_plugins,
            set_plugin_enabled_cmd,
            check_updates_cmd,
            get_settings_cmd,
            set_settings_cmd,
            list_dsh_versions_cmd,
            install_dsh_version_cmd,
            ensure_profile_npmrc_cmd,
            check_deps_cmd,
            fix_deps_cmd,
            scan_junk_cmd,
            clean_junk_cmd,
            export_diag_cmd,
            start_dsh_cmd,
            stop_dsh_cmd,
            find_orphan_nodes_cmd,
            kill_orphans_cmd,
            dsh_status,
            list_running_cmd,
            open_path,
            open_url_cmd,
            toggle_devtools,
            check_update_cmd,
            download_update_cmd,
            launch_installer_and_exit_cmd,
            open_dsh_web_cmd,
            list_browsers_cmd,
            get_env_paths,
            export_profile_cmd,
            import_profile_cmd,
            clone_profile_cmd,
            profile_node_modules_size,
            export_pack_cmd,
            market_packs_cmd,
            download_pack_cmd,
            import_pack_cmd,
            get_profile_notes_cmd,
            save_profile_note_cmd,
            npm_search_cmd,
            npm_package_info_cmd,
            check_compat_cmd,
            market_catalog_cmd,
            fix_build_permit_cmd,
            install_plugin_cmd,
            install_local_plugin_cmd,
            remove_plugin_cmd,
            allow_version_cmd,
            disallow_version_cmd,
            stop_all_dsh_cmd,
            check_portable_runtime_cmd,
            init_portable_runtime_cmd,
        ])
        .setup(|app| {
            // 按物理像素设置窗口初始尺寸：避免高 DPI 缩放下逻辑尺寸被放大
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_size(tauri::PhysicalSize::new(1375, 840));
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_insert_ids_reads_real_ids_from_package_patch() {
        // 模拟 profile/node_modules/<pkg>/package.json + cordis.patch.yml
        let tmp = std::env::temp_dir().join("dshpm-bundle-ids-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let pkg = tmp.join("node_modules").join("dshmarket");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"dshmarket","dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        )
        .unwrap();
        std::fs::write(
            pkg.join("cordis.patch.yml"),
            "- insert:\n    - id: dsh-market\n      name: 'dshmarket'\n",
        )
        .unwrap();
        let ids = bundle_insert_entries(&tmp, "dshmarket");
        assert_eq!(ids, vec![("dsh-market".to_string(), Some("dshmarket".to_string()))]);

        // 多行包：web-all 一类，全部 insert id 都要返回，name 用各自声明的模块名
        let pkg2 = tmp.join("node_modules").join("@linxin666").join("dsh-web-all");
        std::fs::create_dir_all(&pkg2).unwrap();
        std::fs::write(
            pkg2.join("package.json"),
            r#"{"name":"@linxin666/dsh-web-all","dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        )
        .unwrap();
        std::fs::write(
            pkg2.join("cordis.patch.yml"),
            "- insert:\n    - id: web-ui-compat\n      name: '@linxin666/dsh-web-all'\n- insert:\n    - id: web-ui-settings\n      name: '@linxin666/dsh-web-all/settings'\n",
        )
        .unwrap();
        let ids = bundle_insert_entries(&tmp, "@linxin666/dsh-web-all");
        assert_eq!(
            ids,
            vec![
                ("web-ui-compat".to_string(), Some("@linxin666/dsh-web-all".to_string())),
                ("web-ui-settings".to_string(), Some("@linxin666/dsh-web-all/settings".to_string()))
            ]
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn service_ids_prefers_bundle_patch_over_derived() {
        // 推导 id 是 mcp，真实 insert id 是 mcp-config — 必须以真实 id 为准
        let tmp = std::env::temp_dir().join("dshpm-service-ids-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let pkg = tmp.join("node_modules").join("@hyzyn").join("dsh-mcp");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"@hyzyn/dsh-mcp","dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        )
        .unwrap();
        std::fs::write(
            pkg.join("cordis.patch.yml"),
            "- insert:\n    - id: mcp-config\n      name: '@hyzyn/dsh-mcp'\n",
        )
        .unwrap();
        let map = HashMap::new();
        let entries = service_entries_for_plugin(&tmp, &map, "@hyzyn/dsh-mcp");
        assert_eq!(entries[0].0, "mcp-config", "必须以 bundle patch 的 insert id 为首: {entries:?}");
        assert_eq!(entries[0].1.as_deref(), Some("@hyzyn/dsh-mcp"), "name 用 insert 声明的模块名");
        // market 语义：有 insert 行时不加幻影 fallback（避免 all()/some() 被污染）
        assert_eq!(entries.len(), 1, "有 insert 行时只用 insert 行: {entries:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn enable_clears_each_insert_row_by_its_own_name() {
        // codex-ui 这类多行包：两行 insert，name 各不相同
        // 启用时必须按各自 name/id 清 disabled，且不能只删 id 行留下孤儿 name
        let tmp = std::env::temp_dir().join("dshpm-enable-rows-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let pkg = tmp.join("node_modules").join("@michengai").join("dsh-codex-ui");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"name":"@michengai/dsh-codex-ui","dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        )
        .unwrap();
        std::fs::write(
            pkg.join("cordis.patch.yml"),
            "- insert:\n    - id: michengai-codex-ui-session-title\n      name: '@michengai/dsh-codex-ui/session-title'\n    - id: codex-ui\n      name: '@michengai/dsh-codex-ui'\n",
        )
        .unwrap();
        // profile patch：两行都已禁用
        let mut content = String::from(
            "- id: other\n  disabled: true\n\n- id: michengai-codex-ui-session-title\n  name: '@michengai/dsh-codex-ui/session-title'\n  disabled: true\n\n- id: codex-ui\n  name: '@michengai/dsh-codex-ui'\n  disabled: true\n",
        );
        let map = HashMap::new();
        let entries = service_entries_for_plugin(&tmp, &map, "@michengai/dsh-codex-ui");
        assert_eq!(entries.len(), 2);
        // 模拟「启用」
        for (sid, mod_name) in &entries {
            content = patchfile::set_plugin_disabled_for(&content, sid, mod_name.as_deref(), false).unwrap();
        }
        // 两行都应已启用
        for (sid, mod_name) in &entries {
            assert!(
                !patchfile::is_plugin_disabled(&content, sid, mod_name.as_deref()),
                "id={sid} 仍显示禁用: {content}"
            );
        }
        // name 行必须仍归属各自 id 条目（不是孤儿）
        assert!(content.contains("- id: michengai-codex-ui-session-title"), "{content}");
        assert!(content.contains("- id: codex-ui"), "{content}");
        assert!(content.contains("name: '@michengai/dsh-codex-ui/session-title'"), "{content}");
        assert!(content.contains("name: '@michengai/dsh-codex-ui'"), "{content}");
        // 不能再出现这两个 id 的 disabled（other 的 disabled 保留）
        assert!(!content.contains("- id: codex-ui\n  name: '@michengai/dsh-codex-ui'\n  disabled:"), "{content}");
        assert!(!content.contains("session-title'\n  disabled:"), "{content}");
        // 其他条目不动
        assert!(content.contains("- id: other"), "{content}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn copy_dir_all_copies_tree() {
        // 复制整棵目录树（含子目录），统计文件数
        let tmp = std::env::temp_dir().join("dshpm-clone-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), "a").unwrap();
        std::fs::write(src.join("sub").join("b.txt"), "b").unwrap();
        let dst = tmp.join("dst");
        let (n, skipped) = copy_dir_all(&src, &dst).unwrap();
        assert_eq!(n, 2);
        assert_eq!(skipped, 0);
        assert!(dst.join("a.txt").is_file());
        assert!(dst.join("sub").join("b.txt").is_file());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    #[ignore]
    fn real_check_update_no_panic() {
        // 真实网络调用：走镜像失败时回退 VERSION 文件；无论仓库是否有 release，都不 panic、返回结构完整
        let r = tauri::async_runtime::block_on(check_update_cmd());
        assert!(r.is_ok());
        let v = r.unwrap();
        assert!(v.get("current").is_some());
        assert!(v.get("latest").is_some());
        assert!(v.get("hasUpdate").is_some());
        assert!(v.get("url").is_some());
        assert!(v.get("error").is_some());
        println!("check_update = {v}");
    }

    #[test]
    #[ignore]
    fn real_list_dsh_versions_no_panic() {
        // 真实拉取 npm 索引：验证 async + spawn_blocking 后不再 tokio panic、能正常返回版本列表
        let r = tauri::async_runtime::block_on(list_dsh_versions_cmd());
        assert!(r.is_ok(), "列表应返回成功: {r:?}");
        let v = r.unwrap();
        assert!(!v.is_empty(), "应能拿到版本列表");
        println!("拿到的版本数: {}，最新: {:?}", v.len(), v.first().map(|x| x.version.clone()));
    }

    #[test]
    fn stop_all_key_parse() {
        // run_key = env_id::profiles_dir::profile，splitn(3) 后三段齐全
        let key = "env1::C:/users/x/profiles::base";
        let parts: Vec<&str> = key.splitn(3, "::").collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "env1");
        assert_eq!(parts[1], "C:/users/x/profiles");
        assert_eq!(parts[2], "base");
        // profile 名含 :: 时第三段应保留剩余全部
        let key2 = "e::p::a::b";
        let parts2: Vec<&str> = key2.splitn(3, "::").collect();
        assert_eq!(parts2[2], "a::b");
    }

    #[test]
    fn asset_match_rules() {
        assert!(asset_matches("DSH Manager_0.3.13_x64-setup.exe", "0.3.13"));
        assert!(asset_matches("dsh-manager_0.3.13_x64-setup.exe", "0.3.13"));
        assert!(asset_matches("任意名_0.3.13_x64-setup.exe", "0.3.13"));
        assert!(!asset_matches("DSH-Manager-0.3.13-win-x64.exe", "0.3.13"));
        assert!(!asset_matches("DSH Manager_0.3.13_x64-setup.exe.sha256", "0.3.13"));
        assert!(!asset_matches("DSH Manager_0.2.0_x64-setup.exe", "0.3.13"));
        assert!(!asset_matches("DSH Manager_0.3.1_x64-setup.exe", "0.3.13"));
    }

    #[test]
    fn ensure_profile_deps_writes_workspace() {
        let tmp = std::env::temp_dir().join(format!("dshpm-ens-{}", uuid_like("ens")));
        let _ = std::fs::remove_dir_all(&tmp);
        let pdir = tmp.join("profiles");
        std::fs::create_dir_all(pdir.join("x")).unwrap();
        let r = ensure_profile_deps("x", &pdir);
        assert!(r.is_ok());
        let ws = pdir.join("x").join("pnpm-workspace.yaml");
        assert!(ws.is_file());
        let content = std::fs::read_to_string(&ws).unwrap();
        assert!(content.contains("nodeLinker: hoisted"));
        // 已存在时不重复写
        let r2 = ensure_profile_deps("x", &pdir);
        assert!(r2.is_ok());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn builtin_profile_allowed_rules() {
        let tmp = std::env::temp_dir().join(format!("dshpm-t-builtin-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        assert!(builtin_profile_allowed("web", &tmp));
        assert!(builtin_profile_allowed("headless", &tmp));
        assert!(builtin_profile_allowed("sdk-minimal", &tmp));
        assert!(!builtin_profile_allowed("custom", &tmp));
        assert!(!builtin_profile_allowed("init", &tmp));
        std::fs::create_dir_all(tmp.join("web")).unwrap();
        assert!(!builtin_profile_allowed("web", &tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn version_root_parsing() {
        let mk = |cmd: &str, ver: &str, bin: Option<&str>| DshEnv {
            id: "x".into(),
            name: "x".into(),
            source: models::EnvSource::Manual,
            version: ver.into(),
            home_dir: "C:\\Users\\t\\.dsh".into(),
            run_command: cmd.into(),
            bin_path: bin.map(|b| b.to_string()),
            scan_profiles_dir: None,
        };
        // 下载安装的版本目录
        let e = mk(
            "C:\\dsh-versions\\dsh-0.1.7-rc.1\\node_modules\\.bin\\dsh.cmd",
            "0.1.7-rc.1",
            None,
        );
        assert_eq!(
            version_root_of(&e).map(|p| p.to_string_lossy().to_string()),
            Some("C:\\dsh-versions\\dsh-0.1.7-rc.1".into())
        );
        // 全局 CLI（无 dsh- 前缀）→ 无版本根
        let g = mk("dsh", "0.1.0-rc.7", Some("C:\\Users\\t\\AppData\\Roaming\\npm\\dsh.cmd"));
        assert_eq!(version_root_of(&g), None);
        // 扫描目录（run_command 空、bin 为 null）→ 无版本根
        let sc = mk("", "—", None);
        assert_eq!(version_root_of(&sc), None);
        // 版本号带 v 前缀不影响匹配
        let e2 = mk(
            "C:\\dsh-versions\\dsh-0.2.0\\node_modules\\.bin\\dsh.cmd",
            "0.2.0",
            None,
        );
        assert_eq!(
            version_root_of(&e2).map(|p| p.to_string_lossy().to_string()),
            Some("C:\\dsh-versions\\dsh-0.2.0".into())
        );
    }

    #[test]
    fn safe_delete_rules() {
        use std::path::Path;
        let dl = Path::new("C:\\dsh-versions");
        // 不存在/无特征的版本目录 → 不允许（避免误删）
        let v1 = Path::new("C:\\dsh-versions\\dsh-0.1.7-rc.99");
        assert!(!safe_to_delete_version(v1, dl));
        // 下载根本身 → 不允许
        assert!(!safe_to_delete_version(dl, dl));
        // 下载根外 → 不允许
        assert!(!safe_to_delete_version(Path::new("C:\\Users\\t\\dsh-0.1.7-rc.1"), dl));
        // 前缀相近的其他目录（dsh-other）→ 特征检查拦截（无 package.json 等）
        assert!(!safe_to_delete_version(Path::new("C:\\dsh-versions\\dsh-tools"), dl));
        // 父目录包含下载根（下载根在版本根内）→ 不允许
        assert!(!safe_to_delete_version(Path::new("C:\\"), dl));
    }

    #[test]
    fn safe_delete_with_real_dirs() {
        // 真实目录结构验证：版本根 + 特征文件 → 允许删除
        let tmp = std::env::temp_dir().join(format!("dshpm-rm-{}", uuid_like("rm")));
        let _ = std::fs::remove_dir_all(&tmp);
        let dl = tmp.join("versions");
        let ver = dl.join("dsh-0.1.7-rc.1");
        std::fs::create_dir_all(ver.join("node_modules").join(".bin")).unwrap();
        std::fs::write(
            ver.join("node_modules").join(".bin").join("dsh.cmd"),
            "@echo off",
        )
        .unwrap();
        // 特征齐备 → 允许
        assert!(safe_to_delete_version(&ver, &dl));
        // 再验证真实删除
        std::fs::remove_dir_all(&ver).unwrap();
        assert!(!ver.exists());
        // 剩余：无特征目录不允许
        let fake = dl.join("dsh-tools");
        std::fs::create_dir_all(&fake).unwrap();
        assert!(!safe_to_delete_version(&fake, &dl));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn update_asset_naming() {
        let c = update_asset_url_candidates("0.2.0");
        // tag V/v × 文件名点号/空格，4 个候选
        assert!(c.len() >= 4);
        assert!(c.iter().any(|(n, u)| {
            n == "DSH.Manager_0.2.0_x64-setup.exe"
                && u.contains("/releases/download/V0.2.0/")
                && u.contains("DSH.Manager_0.2.0_x64-setup.exe")
        }));
        assert!(c.iter().any(|(n, u)| {
            n == "DSH Manager_0.2.0_x64-setup.exe"
                && u.contains("/releases/download/v0.2.0/")
                && u.contains("DSH%20Manager_0.2.0_x64-setup.exe")
        }));
        // 实际 release 用的点号名
        assert!(asset_matches("DSH.Manager_0.3.13_x64-setup.exe", "0.3.13"));
        // 大小写不敏感
        assert!(asset_matches("dsh.manager_0.3.13_x64-setup.exe", "0.3.13"));
        assert!(asset_matches("DSH.MANAGER_0.3.13_X64-SETUP.EXE", "0.3.13"));
    }

    #[test]
    fn version_compare() {
        assert_eq!(compare_versions("0.2.0", "0.2.0"), 0);
        assert_eq!(compare_versions("0.2.1", "0.2.0"), 1);
        assert_eq!(compare_versions("0.1.9", "0.2.0"), -1);
        assert_eq!(compare_versions("v0.3.13", "0.2.9"), 1);
        assert_eq!(compare_versions("0.2.0-rc.1", "0.2.0"), 0);
        assert_eq!(compare_versions("1.0.0", "0.9.9"), 1);
    }

    #[test]
    fn github_get_text_respects_deadline() {
        use std::time::{Duration, Instant};
        // 已过期的截止时间：立即放弃，不发网络请求
        let (status, msg) = github_get_text_deadline(
            "https://example.invalid/x",
            Instant::now() - Duration::from_millis(1),
        );
        assert_eq!(status, 0);
        assert!(msg.contains("超时"), "{msg}");
    }

    #[test]
    fn expand_download_urls_covers_mirror_and_direct() {
        let urls = expand_download_urls(
            "https://github.com/a/b/releases/download/V1/A_1_x64-setup.exe",
            "https://ghfast.top",
        );
        assert!(urls.iter().any(|u| u.starts_with("https://ghfast.top/")));
        assert!(urls.iter().any(|u| u.contains("gh-proxy.com")));
        assert_eq!(urls.last().unwrap(), "https://github.com/a/b/releases/download/V1/A_1_x64-setup.exe");
    }

    #[test]
    fn scan_dirs_save_and_load_roundtrip() {
        // 保存/加载往返：确保 add_scan_dir 的持久化不会丢数据
        let dir = std::env::temp_dir().join(format!("dshpm-scandirs-{}", uuid_like("t")));
        let _ = std::fs::create_dir_all(&dir);
        // 构造临时 profiles 目录结构：profiles/web（含 package.json）
        let profiles = dir.join("profiles");
        let web = profiles.join("web");
        std::fs::create_dir_all(&web).unwrap();
        std::fs::write(web.join("package.json"), "{}").unwrap();
        // resolve 应命中 profiles 子目录
        let resolved = resolve_profiles_dir(&dir).unwrap();
        assert_eq!(resolved, profiles);
        // 模拟 add_scan_dir 的保存：替换文件路径后保存/加载
        // （scan_dirs_file 是固定路径，这里直接验证 resolve 结果可被 list 使用）
        let ps = scanner::list_profiles_from(&resolved);
        assert!(ps.iter().any(|x| x.name == "web"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_profiles_dir_real_better_harness() {
        // 真实场景：添加单个 profile 目录 -> 应解析到其父 profiles 目录
        let p = std::path::Path::new(
            "C:/Users/xiaolei/.dsh-packs/better-deepseek-harness/profiles/better-deepseek-harness",
        );
        if !p.is_dir() {
            eprintln!("SKIP: better-deepseek-harness 目录不存在");
            return;
        }
        let resolved = resolve_profiles_dir(p).unwrap();
        assert!(
            resolved.to_string_lossy().ends_with("profiles"),
            "应解析到 profiles 目录: {resolved:?}"
        );
        let ps = scanner::list_profiles_from(&resolved);
        let names: Vec<_> = ps.iter().map(|x| x.name.clone()).collect();
        eprintln!("profiles: {names:?}");
        assert!(
            names.iter().any(|n| n == "better-deepseek-harness"),
            "应包含 better-deepseek-harness: {names:?}"
        );
    }

    #[test]
    fn local_folder_spec_strips_verbatim_prefix_and_uses_forward_slashes() {
        // 模拟 Windows canonicalize 输出：\\?\E:\plugin 目录 → E:/plugin（裸绝对路径，正斜杠）
        let tmp = std::env::temp_dir().join(format!("dshpm-spec-{}", uuid_like("folder")));
        let _ = std::fs::create_dir_all(&tmp);
        let spec = local_plugin_spec(tmp.to_string_lossy().as_ref()).unwrap();
        assert!(!spec.contains(r"\\?\"), "不应残留 \\?\\ 前缀: {spec}");
        assert!(!spec.contains('\\'), "不应残留反斜杠: {spec}");
        assert!(spec.starts_with("C:/") || spec.starts_with("D:/"), "应为盘符绝对路径: {spec}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn tgz_spec_passes_through_unchanged() {
        let spec = local_plugin_spec(r"E:\workspace\x\plugin-1.2.2.tgz").unwrap();
        assert_eq!(spec, r"E:\workspace\x\plugin-1.2.2.tgz");
        let spec2 = local_plugin_spec(r"C:\a\b.tar.gz").unwrap();
        assert_eq!(spec2, r"C:\a\b.tar.gz");
    }

    #[test]
    fn unsupported_spec_is_rejected() {
        assert!(local_plugin_spec("C:\\x\\plain.txt").is_err());
    }

    #[test]
    fn run_key_separates_env_and_profile() {
        use std::path::Path;
        let p1 = Path::new("pd1");
        let p2 = Path::new("pd2");
        assert_eq!(run_key("e1", p1, "web"), "e1::pd1::web");
        assert_ne!(
            run_key("e1", p1, "web"),
            run_key("e2", p1, "web"),
            "不同环境同 profile 应区分"
        );
        assert_ne!(
            run_key("e1", p1, "web"),
            run_key("e1", p1, "headless"),
            "同环境不同 profile 应区分"
        );
        assert_ne!(
            run_key("e1", p1, "web"),
            run_key("e1", p2, "web"),
            "同环境不同来源目录同 profile 应区分"
        );
    }

    #[test]
    fn pick_port_uses_requested_when_free() {
        // 找一个当前空闲端口，指定它应原样返回
        let free = runner::find_free_port(40000).unwrap();
        assert_eq!(pick_port(Some(free), &[]).unwrap(), free);
    }

    #[test]
    fn web_ready_when_token_url_or_http_serving() {
        // token URL + 端口通 → 就绪（最稳）
        assert!(compute_web_ready(true, Some("http://127.0.0.1:3080/?token=abc"), false));
        // 无 token 但 HTTP 已服务（浏览器有 cookie 时裸地址能用）→ 也算就绪
        assert!(compute_web_ready(true, Some("http://127.0.0.1:3080"), true));
        assert!(compute_web_ready(true, None, true));
        // 仅 TCP 通、HTTP 未服务且无 token → 不算就绪
        assert!(!compute_web_ready(true, Some("http://127.0.0.1:3080"), false), "仅端口通不算就绪");
        assert!(!compute_web_ready(true, None, false), "无 URL 且未服务不算就绪");
        // 端口不通一律不就绪
        assert!(!compute_web_ready(false, Some("http://127.0.0.1:3080/?token=x"), true));
    }

    #[test]
    fn probe_web_serving_treats_401_as_up() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        // 极简 HTTP 服务：一律回 401（模拟无 cookie 访问 dsh web 裸地址）
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 256];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 4\r\n\r\nauth");
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert!(runner::probe_web_serving(port), "401 响应也算 web 已在服务");
    }

    #[test]
    fn probe_web_serving_false_when_down() {
        let port = runner::find_free_port(45100).unwrap();
        assert!(!runner::probe_web_serving(port), "无服务不应探测为已起");
    }

    #[test]
    fn pick_port_rejects_occupied_request() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let occupied = listener.local_addr().unwrap().port();
        assert!(pick_port(Some(occupied), &[]).is_err(), "占用端口应报错");
    }

    #[test]
    fn pick_port_auto_skips_used_and_occupied() {
        use std::net::TcpListener;
        // 占用两个端口：一个当系统占用，一个当本应用已用；自动分配应避开两者
        let l1 = TcpListener::bind("127.0.0.1:0").unwrap();
        let sys_occ = l1.local_addr().unwrap().port();
        let l2 = TcpListener::bind("127.0.0.1:0").unwrap();
        let app_used = l2.local_addr().unwrap().port();
        let chosen = pick_port(None, &[app_used]).unwrap();
        assert_ne!(chosen, sys_occ);
        assert_ne!(chosen, app_used);
        assert!(chosen >= 3080);
    }

    #[test]
    fn pick_port_zero_means_auto() {
        // 0 与 None 等价：自动分配一个空闲端口
        let p = pick_port(Some(0), &[]).unwrap();
        assert!(!runner::is_port_open(p));
        assert!(p >= 3080);
    }

    #[test]
    fn dsh_home_of_points_to_profiles_parent() {
        // DSH CLI 语义：profile 在 $DSH_HOME/profiles/<name>，所以注入的 home 必须是父目录
        assert_eq!(
            runner::dsh_home_of(std::path::Path::new(r"C:\Users\test\.dsh\profiles")),
            r"C:\Users\test\.dsh"
        );
        assert_eq!(
            runner::dsh_home_of(std::path::Path::new(
                r"C:\Users\test\.dsh-packs\better-deepseek-harness\profiles"
            )),
            r"C:\Users\test\.dsh-packs\better-deepseek-harness"
        );
    }

    #[test]
    fn list_all_profiles_merges_default_and_scan_dirs() {
        // 真实目录验证：默认 ~/.dsh/profiles + 扫描目录（harness）合并，各带来源目录
        let home = scanner::default_dsh_home();
        let default_pd = home.join("profiles");
        if !default_pd.is_dir() {
            return; // 非本机环境跳过
        }
        let scan_pd = r"C:\Users\xiaolei\.dsh-packs\better-deepseek-harness\profiles";
        let state = AppState {
            starting: std::sync::Mutex::new(std::collections::HashSet::new()),
            manual_envs: std::sync::Mutex::new(Vec::new()),
            scan_dirs: std::sync::Mutex::new(vec![crate::models::ScanDirEntry {
                id: "scan-test".into(),
                path: r"C:\Users\xiaolei\.dsh-packs\better-deepseek-harness".into(),
                label: "harness".into(),
                profiles_dir: scan_pd.into(),
                dsh_bin: None,
            }]),
            running: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        let scans = state.scan_dirs.lock().unwrap().clone();
        let all = list_all_profiles_inner(&scans);
        let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
        // 默认目录的 profile 应带默认来源
        assert!(names.contains(&"web"), "应包含默认 profile web，实际: {names:?}");
        let web = all.iter().find(|p| p.name == "web").unwrap();
        assert_eq!(web.profiles_dir, default_pd.to_string_lossy());
        // 扫描目录的 profile 应带扫描来源
        if std::path::Path::new(scan_pd).join("better-deepseek-harness").is_dir() {
            assert!(
                names.contains(&"better-deepseek-harness"),
                "应包含扫描目录 profile，实际: {names:?}"
            );
            let hp = all.iter().find(|p| p.name == "better-deepseek-harness").unwrap();
            assert_eq!(hp.profiles_dir, scan_pd);
        }
        // 来源目录去重：同一目录不应出现两份同名 profile
        let mut seen = std::collections::HashSet::new();
        for p in &all {
            let k = format!("{}::{}", p.profiles_dir, p.name);
            let inserted = seen.insert(k.clone());
            assert!(inserted, "重复 profile: {k}");
        }
    }
}
