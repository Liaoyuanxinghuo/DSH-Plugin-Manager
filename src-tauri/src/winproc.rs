//! 原生 Windows 进程探测（Toolhelp32 快照 + OpenProcess）。
//!
//! 设计目标：**不 spawn 子进程**。旧实现用 `tasklist` / `powershell` 探测进程，
//! 每 3 秒轮询都要起 1-2 个子进程（每个 30-80ms、上百 MB 内存抖动），是 UI 卡顿与
//! CPU 尖峰的元凶。这里直接调 Win32 API，单次探测开销在微秒级。
//!
//! 全部 unsafe 集中在本模块，对外只暴露安全封装。

#![allow(non_snake_case)]

use std::collections::HashSet;

#[cfg(windows)]
mod ffi {
    use std::os::raw::c_void;

    pub type HANDLE = *mut c_void;
    pub type BOOL = i32;
    pub type DWORD = u32;

    pub const TH32CS_SNAPPROCESS: DWORD = 0x0000_0002;
    pub const PROCESS_QUERY_LIMITED_INFORMATION: DWORD = 0x1000;
    pub const STILL_ACTIVE: DWORD = 259;
    pub const INVALID_HANDLE_VALUE: isize = -1;

    /// PROCESSENTRY32W（Win32 结构；th32DefaultHeapID 为 ULONG_PTR = usize）
    #[repr(C)]
    pub struct PROCESSENTRY32W {
        pub dwSize: DWORD,
        pub cntUsage: DWORD,
        pub th32ProcessID: DWORD,
        pub th32DefaultHeapID: usize,
        pub th32ModuleID: DWORD,
        pub cntThreads: DWORD,
        pub th32ParentProcessID: DWORD,
        pub pcPriClassBase: i32,
        pub dwFlags: DWORD,
        pub szExeFile: [u16; 260],
    }

    extern "system" {
        pub fn CreateToolhelp32Snapshot(dwFlags: DWORD, th32ProcessID: DWORD) -> HANDLE;
        pub fn Process32FirstW(hSnapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> BOOL;
        pub fn Process32NextW(hSnapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> BOOL;
        pub fn CloseHandle(hObject: HANDLE) -> BOOL;
        pub fn OpenProcess(
            dwDesiredAccess: DWORD,
            bInheritHandle: BOOL,
            dwProcessId: DWORD,
        ) -> HANDLE;
        pub fn GetExitCodeProcess(hProcess: HANDLE, lpExitCode: *mut DWORD) -> BOOL;
        pub fn QueryFullProcessImageNameW(
            hProcess: HANDLE,
            dwFlags: DWORD,
            lpExeName: *mut u16,
            lpdwSize: *mut DWORD,
        ) -> BOOL;
    }
}

/// 快照中的一条进程记录
#[derive(Debug, Clone)]
pub struct ProcEntry {
    pub pid: u32,
    /// 父进程 PID（用于识别 Electron/浏览器子进程的主进程）
    pub ppid: u32,
    /// 可执行文件名（如 `node.exe`，不含目录）
    pub name: String,
}

/// 一次进程表快照（pid + 父 pid + 映像名）。
#[cfg(windows)]
pub fn snapshot() -> Vec<ProcEntry> {
    use ffi::*;
    let mut out = Vec::new();
    unsafe {
        let h = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if h as isize == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            cntUsage: 0,
            th32ProcessID: 0,
            th32DefaultHeapID: 0,
            th32ModuleID: 0,
            cntThreads: 0,
            th32ParentProcessID: 0,
            pcPriClassBase: 0,
            dwFlags: 0,
            szExeFile: [0; 260],
        };
        let mut ok = Process32FirstW(h, &mut entry);
        while ok != 0 {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            out.push(ProcEntry {
                pid: entry.th32ProcessID,
                ppid: entry.th32ParentProcessID,
                name: String::from_utf16_lossy(&entry.szExeFile[..len]),
            });
            ok = Process32NextW(h, &mut entry);
        }
        CloseHandle(h);
    }
    out
}

#[cfg(not(windows))]
pub fn snapshot() -> Vec<ProcEntry> {
    Vec::new()
}

/// 进程是否存活：OpenProcess(QUERY_LIMITED_INFORMATION) + GetExitCodeProcess == STILL_ACTIVE。
/// 比 `tasklist` 快 3 个数量级，且不产生任何子进程。
#[cfg(windows)]
pub fn is_pid_alive(pid: u32) -> bool {
    use ffi::*;
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code: DWORD = 0;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        ok != 0 && code == STILL_ACTIVE
    }
}

#[cfg(not(windows))]
pub fn is_pid_alive(_pid: u32) -> bool {
    false
}

/// 批量探测存活 PID 集合（单次快照；快照失败退回逐个 OpenProcess，同样无子进程）。
pub fn alive_pids(pids: &[u32]) -> HashSet<u32> {
    let mut alive = HashSet::new();
    if pids.is_empty() {
        return alive;
    }
    let wanted: HashSet<u32> = pids.iter().copied().collect();
    let procs = snapshot();
    if procs.is_empty() {
        // 快照不可用（极少数受限环境）：退回逐个探测
        for &p in pids {
            if is_pid_alive(p) {
                alive.insert(p);
            }
        }
        return alive;
    }
    for e in procs {
        if wanted.contains(&e.pid) {
            alive.insert(e.pid);
        }
    }
    alive
}

/// 进程映像绝对路径（QueryFullProcessImageNameW）。PID 复用/权限不足时返回 None。
#[cfg(windows)]
pub fn image_path(pid: u32) -> Option<String> {
    use ffi::*;
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 32768];
        let mut size = buf.len() as DWORD;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(h);
        if ok == 0 || size == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..size as usize]))
    }
}

#[cfg(not(windows))]
pub fn image_path(_pid: u32) -> Option<String> {
    None
}

/// 映像文件名（不含目录，小写）。用于 PID 复用校验。
pub fn image_name(pid: u32) -> Option<String> {
    image_path(pid).map(|p| {
        p.rsplit('\\')
            .next()
            .unwrap_or("")
            .to_string()
            .to_lowercase()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        let pid = std::process::id();
        assert!(is_pid_alive(pid));
        assert!(alive_pids(&[pid]).contains(&pid));
    }

    #[test]
    fn nonexistent_pid_is_not_alive() {
        // 0xFFFF_FFFF 不是合法 PID（Windows 最大 PID 远小于此）
        assert!(!is_pid_alive(u32::MAX - 1));
        assert!(!alive_pids(&[u32::MAX - 1]).contains(&(u32::MAX - 1)));
    }

    #[test]
    fn snapshot_contains_current_process() {
        let pid = std::process::id();
        let procs = snapshot();
        assert!(procs.iter().any(|p| p.pid == pid), "快照应包含当前进程");
        // 映像名探测（cargo 测试宿主可能是 cargo.exe / test 二进制，只要求有值）
        assert!(image_name(pid).is_some());
    }

    #[test]
    fn empty_pid_list_is_cheap() {
        assert!(alive_pids(&[]).is_empty());
    }
}
