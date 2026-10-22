//! 运行实例记账（running ledger）：**启动写账、停止销账**。
//!
//! 账目落盘在 `data_dir/dsh-plugin-manager/running_ledger.json`。软件退出时
//! （包括用户主动「放生 DSH」、软件崩溃/被任务管理器结束）账目保留，
//! 重启软件后由 `adopt_released` 按账目接管仍在运行的 DSH：
//! 状态/按钮自动回到「运行中」，可直接「打开界面」或「停止」。
//!
//! 只记本软件启动的实例——不会误接管你在终端手动跑的 dsh。

use crate::models::RunningProcess;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 串行化读写（多 command 并发写账时避免互相覆盖）
static IO_LOCK: Mutex<()> = Mutex::new(());

/// 记账文件路径
pub fn ledger_file() -> PathBuf {
    let dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    dir.join("dsh-plugin-manager").join("running_ledger.json")
}

fn load_from(path: &Path) -> HashMap<String, RunningProcess> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_to(path: &Path, map: &HashMap<String, RunningProcess>) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建记账目录失败: {e}"))?;
    }
    let json = serde_json::to_string_pretty(map).map_err(|e| format!("序列化记账失败: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("写记账文件失败: {e}"))
}

/// 读全部账目（key -> 运行记录）
pub fn load() -> HashMap<String, RunningProcess> {
    let _g = IO_LOCK.lock().unwrap();
    load_from(&ledger_file())
}

/// 全量覆盖写账
pub fn save(map: &HashMap<String, RunningProcess>) -> Result<(), String> {
    let _g = IO_LOCK.lock().unwrap();
    save_to(&ledger_file(), map)
}

/// 写/更新单条账目（启动成功时调用）
pub fn upsert(key: &str, proc: &RunningProcess) -> Result<(), String> {
    let _g = IO_LOCK.lock().unwrap();
    let path = ledger_file();
    let mut map = load_from(&path);
    map.insert(key.to_string(), proc.clone());
    save_to(&path, &map)
}

/// 销单条账目（停止成功 / 确认进程已退出时调用）
pub fn remove(key: &str) -> Result<(), String> {
    let _g = IO_LOCK.lock().unwrap();
    let path = ledger_file();
    let mut map = load_from(&path);
    if map.remove(key).is_some() {
        return save_to(&path, &map);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pid: u32) -> RunningProcess {
        RunningProcess {
            env_id: "e1".into(),
            profiles_dir: "C:\\Users\\t\\.dsh\\profiles".into(),
            profile: "web".into(),
            pid,
            port: 3080,
            started_at: "2026-02-20 10:00:00".into(),
            url: "http://127.0.0.1:3080".into(),
            log_path: "C:\\tmp\\dsh.log".into(),
            adopted: false,
        }
    }

    #[test]
    fn ledger_roundtrip_preserves_fields() {
        let dir = std::env::temp_dir().join(format!("dsh-runlog-{}", std::process::id()));
        let path = dir.join("running_ledger.json");
        let mut map = HashMap::new();
        map.insert("e1::C:\\Users\\t\\.dsh\\profiles::web".to_string(), sample(4242));
        save_to(&path, &map).unwrap();
        let back = load_from(&path);
        let p = back.get("e1::C:\\Users\\t\\.dsh\\profiles::web").unwrap();
        assert_eq!(p.pid, 4242);
        assert_eq!(p.port, 3080);
        assert_eq!(p.profile, "web");
        assert!(!p.adopted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_ledger_is_empty() {
        let p = std::env::temp_dir().join(format!("dsh-runlog-none-{}.json", std::process::id()));
        assert!(load_from(&p).is_empty());
    }

    #[test]
    fn old_ledger_without_adopted_field_loads() {
        // 旧版本账目没有 adopted 字段 → serde(default) 兼容为 false
        let dir = std::env::temp_dir().join(format!("dsh-runlog-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("running_ledger.json");
        std::fs::write(
            &path,
            r#"{"k":{"envId":"e1","profilesDir":"C:\\p","profile":"web","pid":1,"port":2,"startedAt":"t","url":"u","logPath":"l"}}"#,
        )
        .unwrap();
        let m = load_from(&path);
        assert!(m.get("k").unwrap().adopted == false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("dsh-runlog-rm-{}", std::process::id()));
        let path = dir.join("running_ledger.json");
        let mut map = HashMap::new();
        map.insert("k".to_string(), sample(1));
        save_to(&path, &map).unwrap();
        let mut m = load_from(&path);
        assert!(m.remove("k").is_some());
        assert!(m.remove("k").is_none());
        save_to(&path, &m).unwrap();
        assert!(load_from(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
