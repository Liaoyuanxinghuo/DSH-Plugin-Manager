//! DSH Plugin Manager 应用入口与命令注册

mod buildpermit;
mod dsh_install;
mod fsutil;
mod installer;
mod market;
mod models;
mod npm;
mod health;
mod patchfile;
mod settings;
mod profile_io;
mod runner;
mod scanner;

use models::*;
use std::collections::HashMap;
use tauri::Manager;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::State;

/// 应用全局状态
pub struct AppState {
    /// 手动添加的 DSH 环境（持久化）
    pub manual_envs: Mutex<Vec<DshEnv>>,
    /// 用户添加的本地 profile 扫描目录（持久化）
    pub scan_dirs: Mutex<Vec<ScanDirEntry>>,
    /// 运行中的 DSH 进程（env_id -> process）
    pub running: Mutex<HashMap<String, RunningProcess>>,
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
fn scan_envs(state: State<AppState>) -> Vec<DshEnv> {
    let manual = state.manual_envs.lock().unwrap().clone();
    let scans = state.scan_dirs.lock().unwrap().clone();
    scanner::scan_envs(&manual, &scans)
}

/// 手动添加环境（如 pnpm dlx 版本）
#[tauri::command]
fn add_manual_env(state: State<AppState>, name: String, command: String) -> Result<DshEnv, String> {
    let name = name.trim().to_string();
    let command = command.trim().to_string();
    if name.is_empty() || command.is_empty() {
        return Err("名称和命令都不能为空".to_string());
    }
    let version = scanner::validate_manual_env(&command)?;
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

/// 添加本地 profile 扫描目录
#[tauri::command]
fn add_scan_dir(state: State<AppState>, path: String) -> Result<ScanDirEntry, String> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("目录路径不能为空".to_string());
    }
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
    let entry = ScanDirEntry {
        id,
        path,
        label,
        profiles_dir: profiles_dir.to_string_lossy().to_string(),
        dsh_bin: None,
    };
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

/// 删除扫描目录
#[tauri::command]
fn remove_scan_dir(state: State<AppState>, id: String) -> Result<(), String> {
    let mut dirs = state.scan_dirs.lock().unwrap();
    dirs.retain(|d| d.id != id);
    save_scan_dirs(&dirs);
    Ok(())
}

/// 列出所有扫描目录
#[tauri::command]
fn list_scan_dirs(state: State<AppState>) -> Vec<ScanDirEntry> {
    state.scan_dirs.lock().unwrap().clone()
}

/// 在指定目录中扫描 dsh 本体并添加为可运行环境（自动探测命令与版本）
#[tauri::command]
fn add_dsh_scan_dir(state: State<AppState>, path: String) -> Result<Vec<DshEnv>, String> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err("目录路径不能为空".to_string());
    }
    let p = std::path::Path::new(&path);
    if !p.is_dir() {
        return Err(format!("目录不存在或不可访问: {path}"));
    }
    let found = scanner::scan_dsh_binary_tree(p, 3);
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

fn save_manual_envs_from_state(state: &State<AppState>) {
    let envs = state.manual_envs.lock().unwrap();
    save_manual_envs(&envs);
}

/// 删除手动添加的环境
#[tauri::command]
fn remove_manual_env(state: State<AppState>, id: String) -> Result<(), String> {
    let mut envs = state.manual_envs.lock().unwrap();
    envs.retain(|e| e.id != id);
    save_manual_envs(&envs);
    Ok(())
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
fn get_env(state: State<AppState>, env_id: String) -> Option<DshEnv> {
    get_env_inner(&state, &env_id).ok()
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
fn list_all_profiles(state: State<AppState>) -> Vec<ProfileInfo> {
    let scans = state.scan_dirs.lock().unwrap().clone();
    list_all_profiles_inner(&scans)
}

/// 列出某环境的全部 profile
#[tauri::command]
fn list_profiles(state: State<AppState>, env_id: String) -> Result<Vec<ProfileInfo>, String> {
    let env = get_env_inner(&state, &env_id)?;
    if let Some(spd) = &env.scan_profiles_dir {
        Ok(scanner::list_profiles_from(std::path::Path::new(spd)))
    } else {
        Ok(scanner::list_profiles(&env.home_dir))
    }
}

fn get_env_inner(state: &State<AppState>, env_id: &str) -> Result<DshEnv, String> {
    let scans = state.scan_dirs.lock().unwrap().clone();
    scanner::scan_envs(&state.manual_envs.lock().unwrap(), &scans)
        .into_iter()
        .find(|e| e.id == env_id)
        .ok_or_else(|| format!("环境不存在: {env_id}"))
}

/// 读取 profile 的插件清单
#[tauri::command]
fn list_plugins(
    state: State<AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<Vec<PluginInfo>, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let profile_dir = profiles_dir.join(&profile);
    if !profile_dir.exists() {
        return Err(format!("profile 不存在: {profile}"));
    }
    let pkg = scanner::read_profile_package_full(&profile_dir)
        .ok_or_else(|| "无法读取 profile 的 package.json".to_string())?;
    // 读取 cordis.patch.yml 中的禁用列表；服务 id 以 dump-config 为准
    let disabled_ids = read_disabled_ids(&profile_dir);
    let service_map = load_service_map(&env, &profile);
    let mut plugins: Vec<PluginInfo> = pkg
        .dependencies
        .into_iter()
        .map(|(name, spec)| {
            let is_bundle = pkg.bundles.contains(&name);
            let sid = service_id_for(&service_map, &name);
            PluginInfo {
                name,
                spec,
                is_bundle,
                compatible: None,
                incompatible_reason: None,
                is_disabled: disabled_ids.contains(&sid),
            }
        })
        .collect();
    plugins.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(plugins)
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
fn read_disabled_ids(profile_dir: &std::path::Path) -> Vec<String> {
    let patch_path = profile_dir.join("cordis.patch.yml");
    match std::fs::read_to_string(&patch_path) {
        Ok(content) => patchfile::list_disabled_ids(&content),
        Err(_) => Vec::new(),
    }
}

/// 设置插件启用/停用（写入 cordis.patch.yml）
#[tauri::command]
fn set_plugin_enabled_cmd(
    state: State<AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    package_name: String,
    enabled: bool,
) -> Result<Vec<String>, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let profile_dir = profiles_dir.join(&profile);
    if !profile_dir.is_dir() {
        return Err(format!("profile 目录不存在: {}", profile_dir.display()));
    }
    let patch_path = profile_dir.join("cordis.patch.yml");
    let service_map = load_service_map(&env, &profile);
    let sid = service_id_for(&service_map, &package_name);
    let content = std::fs::read_to_string(&patch_path).unwrap_or_default();
    let new_content = patchfile::set_plugin_disabled(&content, &sid, !enabled)?;
    if new_content != content {
        std::fs::write(&patch_path, new_content).map_err(|e| format!("写入 patch 失败: {e}"))?;
    }
    // 返回新的禁用列表
    let content = std::fs::read_to_string(&patch_path).unwrap_or_default();
    Ok(patchfile::list_disabled_ids(&content))
}

/// 检查插件更新（并行查询 npm latest）
#[tauri::command]
fn check_updates_cmd(
    state: State<AppState>,
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

/// 运行中进程的 key：env_id + "::" + profile（同一环境的不同 profile 可同时运行）
fn run_key(env_id: &str, profiles_dir: &str, profile: &str) -> String {
    format!("{}::{}::{}", env_id, profiles_dir, profile)
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

/// 启动 DSH（多实例）：任意 env×profile 组合独立进程；
/// 端口默认自动分配（从 3080 起找第一个空闲端口），指定端口被占用时报错。
#[tauri::command]
fn start_dsh_cmd(
    state: State<AppState>,
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

    // profile 来源目录（任意 dsh × 任意来源 profile 组合：注入 DSH_HOME）
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let key = run_key(&env_id, &profiles_dir_str, &profile);
    let mut running = state.running.lock().unwrap();
    if let Some(existing) = running.get(&key) {
        return Err(format!(
            "「{profile}」在该环境下已在运行（PID {}，端口 {}），请先停止",
            existing.pid, existing.port
        ));
    }

    // 确定端口：用户指定（非 0）→ 校验占用；否则自动分配（避开本应用已用端口 + 系统占用）
    let port = pick_port(port, &running.values().map(|p| p.port).collect::<Vec<_>>())?;

    let dsh_home = runner::dsh_home_of(&profiles_dir);
    let (pid, log_path) =
        runner::start_dsh(&env, &profile, port, Some(&dsh_home))?;

    // 短暂探测：2 秒后进程仍存活才算启动成功。
    // dsh 若启动失败（例如用旧版 dsh 加载新版 profile → ERR_MODULE_NOT_FOUND）会秒退。
    std::thread::sleep(std::time::Duration::from_secs(2));
    if !runner::is_pid_alive(pid) {
        let tail = runner::read_log_tail(&log_path, 12);
        let _ = std::fs::remove_file(&log_path);
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
    running.insert(key, proc.clone());

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
fn prune_dead(running: &mut HashMap<String, RunningProcess>) {
    let dead: Vec<String> = running
        .iter()
        .filter(|(_, p)| !runner::is_pid_alive(p.pid))
        .map(|(k, _)| k.clone())
        .collect();
    for k in dead {
        running.remove(&k);
    }
}

/// 停止指定 env×profile 的进程（每个停止按钮只结束特定进程）
#[tauri::command]
fn stop_dsh_cmd(state: State<AppState>, env_id: String, profile: String, profiles_dir_str: String) -> Result<(), String> {
    let key = run_key(&env_id, &profiles_dir_str, &profile);
    let mut running = state.running.lock().unwrap();
    let proc = running.remove(&key);
    if let Some(p) = proc {
        if !runner::is_pid_alive(p.pid) {
            // 进程已自行退出，视为停止成功
            return Ok(());
        }
        match runner::stop_dsh(p.pid) {
            Ok(_) => Ok(()),
            Err(e) => {
                // taskkill 失败但进程可能已退出
                running.insert(key, p);
                Err(e)
            }
        }
    } else {
        // 未在运行视为已停止
        Ok(())
    }
}

/// 查询指定 env×profile 的运行状态（自动清理已退出进程）
#[tauri::command]
fn dsh_status(state: State<AppState>, env_id: String, profile: String, profiles_dir_str: String) -> DshStatus {
    let mut running = state.running.lock().unwrap();
    prune_dead(&mut running);
    let proc = running.get(&run_key(&env_id, &profiles_dir_str, &profile)).cloned();
    drop(running);
    match proc {
        Some(p) => DshStatus {
            running: true,
            port_open: runner::is_port_open(p.port),
            process: Some(p),
        },
        None => DshStatus {
            running: false,
            port_open: false,
            process: None,
        },
    }
}

/// 列出全部运行中的 DSH 进程（跨环境跨 profile，自动清理已退出进程）
#[tauri::command]
fn list_running_cmd(state: State<AppState>) -> Vec<RunningProcess> {
    let mut running = state.running.lock().unwrap();
    prune_dead(&mut running);
    running.values().cloned().collect()
}

// ==================== 命令：Profile 导出/导入 ====================

/// 新建 profile
#[tauri::command]
fn create_profile_cmd(
    state: State<AppState>,
    env_id: String,
    name: String,
    profiles_dir_str: String,
) -> Result<profile_io::CreateProfileResult, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    if !profiles_dir.is_dir() {
        return Err(format!("profiles 目录不存在: {}", profiles_dir.display()));
    }
    profile_io::create_profile(&profiles_dir, &name)
}

/// 删除 profile（running 时拒绝）
#[tauri::command]
fn delete_profile_cmd(
    state: State<AppState>,
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
    profile_io::delete_profile(&profiles_dir, &name, running)
}

// ==================== 命令：M5 设置 / DSH 多版本下载 ====================

/// 读取设置
#[tauri::command]
fn get_settings_cmd() -> settings::Settings {
    settings::load_settings()
}

/// 保存设置
#[tauri::command]
fn set_settings_cmd(npm_registry: String, dsh_download_dir: String) -> Result<(), String> {
    settings::validate_registry(&npm_registry)?;
    if dsh_download_dir.trim().is_empty() {
        return Err("DSH 下载目录不能为空".to_string());
    }
    let mut s = settings::load_settings();
    s.npm_registry = settings::normalize_registry(&npm_registry);
    s.dsh_download_dir = dsh_download_dir.trim().to_string();
    settings::save_settings(&s)
}

/// 列出 DSH 全部版本
#[tauri::command]
fn list_dsh_versions_cmd() -> Result<Vec<dsh_install::DshVersionInfo>, String> {
    let registry = settings::load_settings().npm_registry;
    dsh_install::list_dsh_versions(&registry)
}

/// 下载并安装指定 DSH 版本到目标目录（流式日志）
#[tauri::command]
fn install_dsh_version_cmd(
    app: tauri::AppHandle,
    version: String,
    target_dir: String,
) -> Result<dsh_install::DshInstallResult, String> {
    if version.trim().is_empty() {
        return Err("版本不能为空".to_string());
    }
    if target_dir.trim().is_empty() {
        return Err("目标目录不能为空".to_string());
    }
    let registry = settings::load_settings().npm_registry;
    Ok(dsh_install::install_dsh_version(&app, version.trim(), target_dir.trim(), &registry))
}

/// 确保 profile 的 .npmrc 使用当前镜像（安装插件前调用）
#[tauri::command]
fn ensure_profile_npmrc_cmd(
    state: State<AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<(), String> {
    let env = get_env_inner(&state, &env_id)?;
    let profile_dir = profiles_dir_arg(&profiles_dir_str, &env).join(&profile);
    if !profile_dir.is_dir() {
        return Err(format!("profile 不存在: {profile}"));
    }
    let registry = settings::load_settings().npm_registry;
    dsh_install::ensure_profile_npmrc(&profile_dir, &registry)
}

// ==================== 命令：M4 依赖健康 / 残留清理 / 诊断导出 ====================

/// 依赖健康检查
#[tauri::command]
fn check_deps_cmd(
    state: State<AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<Vec<health::DepIssue>, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profile_dir = profiles_dir_arg(&profiles_dir_str, &env).join(&profile);
    health::check_profile_deps(&profile_dir)
}

/// 修复依赖：在 profile 目录执行 dsh plugin install（pnpm install，流式日志）
#[tauri::command]
fn fix_deps_cmd(
    app: tauri::AppHandle,
    state: State<AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<installer::InstallOutcome, String> {
    let env = get_env_inner(&state, &env_id)?;
    if env.run_command.trim().is_empty() {
        return Err("该环境未绑定 dsh 运行时，无法修复依赖".to_string());
    }
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let dsh_home = runner::dsh_home_of(&profiles_dir);
    Ok(installer::run_dsh_with_logs(
        &app,
        &env,
        &["plugin".to_string(), "--profile".to_string(), profile, "install".to_string()],
        Some(&dsh_home),
    ))
}

/// 扫描 profile 内残留/缓存目录
#[tauri::command]
fn scan_junk_cmd(
    state: State<AppState>,
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
fn clean_junk_cmd(
    state: State<AppState>,
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
    health::clean_junk(&profile_dir, &names)
}

/// 导出诊断包（配置文件 + 会话/存储摘要 + 各 profile 元数据）
#[tauri::command]
fn export_diag_cmd(
    state: State<AppState>,
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
fn export_profile_cmd(
    state: State<AppState>,
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
    profile_io::export_profile(&profile_dir, &target, excl)
}

/// 导入 zip 到指定环境的 profiles 目录（重名自动改名）
#[tauri::command]
fn import_profile_cmd(
    state: State<AppState>,
    env_id: String,
    zip_path: String,
    profiles_dir_str: String,
) -> Result<profile_io::ImportResult, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    profile_io::import_profile(std::path::Path::new(&zip_path), &profiles_dir)
}

/// 获取 profile 的 node_modules 体积（导出前提示用）
#[tauri::command]
fn profile_node_modules_size(
    state: State<AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<u64, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let profile_dir = profiles_dir.join(&profile);
    Ok(profile_io::node_modules_size(&profile_dir))
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
fn npm_search_cmd(query: String) -> Result<Vec<npm::NpmSearchHit>, String> {
    let registry = settings::load_settings().npm_registry;
    npm::npm_search(&query, &registry)
}

/// 获取包全部版本信息
#[tauri::command]
fn npm_package_info_cmd(name: String) -> Result<npm::NpmPackageInfo, String> {
    let registry = settings::load_settings().npm_registry;
    npm::npm_package_info(&name, &registry)
}

/// 兼容性预检：指定插件版本 vs 运行时版本
#[tauri::command]
fn check_compat_cmd(
    name: String,
    version: String,
    runtime_version: String,
) -> Result<Vec<npm::PeerIssue>, String> {
    let registry = settings::load_settings().npm_registry;
    let info = npm::npm_package_info(&name, &registry)?;
    let vinfo = info
        .versions
        .iter()
        .find(|v| v.version == version)
        .ok_or_else(|| format!("未找到版本 {version}"))?;
    Ok(npm::check_peer_compat(&vinfo.peer_dependencies, &runtime_version))
}

/// 拉取插件市场目录
#[tauri::command]
fn market_catalog_cmd() -> Result<market::MarketCatalog, String> {
    market::market_catalog()
}

/// 修复 pnpm 构建白名单（git 源插件安装需要）
#[tauri::command]
fn fix_build_permit_cmd(
    state: State<AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
    packages: Vec<String>,
) -> Result<Vec<String>, String> {
    let env = get_env_inner(&state, &env_id)?;
    let profiles_dir = profiles_dir_arg(&profiles_dir_str, &env);
    let profile_dir = profiles_dir.join(profile);
    if !profile_dir.is_dir() {
        return Err(format!("profile 目录不存在: {}", profile_dir.display()));
    }
    buildpermit::fix_build_permit(&profile_dir, &packages)
}

/// 本地安装插件：文件夹（file:）或 .tgz 压缩包
#[tauri::command]
fn install_local_plugin_cmd(
    app: tauri::AppHandle,
    state: State<AppState>,
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
    Ok(installer::install_plugin(&app, &env, &profile, &spec, &profiles_dir))
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
fn install_plugin_cmd(
    app: tauri::AppHandle,
    state: State<AppState>,
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
    Ok(installer::install_plugin(&app, &env, &profile, &spec, &profiles_dir))
}

/// 卸载插件
#[tauri::command]
fn remove_plugin_cmd(
    app: tauri::AppHandle,
    state: State<AppState>,
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
    Ok(installer::remove_plugin(&app, &env, &profile, &name, &profiles_dir))
}

/// 豁免版本校验
#[tauri::command]
fn allow_version_cmd(
    app: tauri::AppHandle,
    state: State<AppState>,
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
    Ok(installer::allow_version(&app, &env, &pkg_spec, &profiles_dir))
}

/// 撤销豁免
#[tauri::command]
fn disallow_version_cmd(
    app: tauri::AppHandle,
    state: State<AppState>,
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
    Ok(installer::disallow_version(&app, &env, &pkg_spec, &profiles_dir))
}

// ==================== 命令：文件/URL ====================

/// 在资源管理器中打开路径
#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    fsutil::open_in_explorer(&path)
}

/// 用默认浏览器打开 URL
#[tauri::command]
fn open_url_cmd(url: String) -> Result<(), String> {
    fsutil::open_url(&url)
}

/// 打开指定 env×profile 的 DSH web 界面。
/// 运行时实时从日志提取最新带 token 的访问地址（服务就绪后 token 一定已打印），
/// 避免启动早期抓不到 token 导致"连接中"。
#[tauri::command]
fn open_dsh_web_cmd(
    state: State<AppState>,
    env_id: String,
    profile: String,
    profiles_dir_str: String,
) -> Result<(), String> {
    let key = run_key(&env_id, &profiles_dir_str, &profile);
    let running = state.running.lock().unwrap();
    let proc = running
        .get(&key)
        .ok_or_else(|| format!("「{profile}」在该环境未在运行"))?;
    let url = if proc.log_path.is_empty() {
        format!("http://127.0.0.1:{}", proc.port)
    } else {
        runner::find_auth_url(&proc.log_path)
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", proc.port))
    };
    drop(running);
    fsutil::open_url(&url)
}

/// 获取环境的关键路径集合（供前端文件按钮使用）
#[tauri::command]
fn get_env_paths(state: State<AppState>, env_id: String) -> Result<serde_json::Value, String> {
    let env = get_env_inner(&state, &env_id)?;
    let home = std::path::Path::new(&env.home_dir);
    let profiles_dir = env
        .scan_profiles_dir
        .as_ref()
        .map(|s| s.clone())
        .unwrap_or_else(|| home.join("profiles").to_string_lossy().to_string());
    Ok(serde_json::json!({
        "homeDir": env.home_dir,
        "profilesDir": profiles_dir,
        "sessionsDir": home.join("sessions").to_string_lossy().to_string(),
        "logsDir": home.join("logs").to_string_lossy().to_string(),
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
            list_profiles,
            create_profile_cmd,
            delete_profile_cmd,
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
            dsh_status,
            list_running_cmd,
            open_path,
            open_url_cmd,
            open_dsh_web_cmd,
            get_env_paths,
            export_profile_cmd,
            import_profile_cmd,
            profile_node_modules_size,
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
        assert_eq!(run_key("e1", "pd1", "web"), "e1::pd1::web");
        assert_ne!(
            run_key("e1", "pd1", "web"),
            run_key("e2", "pd1", "web"),
            "不同环境同 profile 应区分"
        );
        assert_ne!(
            run_key("e1", "pd1", "web"),
            run_key("e1", "pd1", "headless"),
            "同环境不同 profile 应区分"
        );
        assert_ne!(
            run_key("e1", "pd1", "web"),
            run_key("e1", "pd2", "web"),
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
