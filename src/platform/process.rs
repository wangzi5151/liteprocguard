//! 跨平台进程枚举与进程控制。
//!
//! * Linux / Termux：直接解析 `/proc`，零依赖、零库。
//! * Windows：ToolHelp32 快照 + OpenProcess。
//! * 其它平台：返回空列表（功能降级但不崩溃）。
//!
//! CPU 占比统一定义为“占整机总容量的百分比”（0-100），
//! 便于和规则里的 `cpu_limit_percent` 直接比较。

use crate::core::model::{Priority, ProcessInfo};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// 一次 CPU 采样。
#[derive(Clone, Copy)]
struct Sample {
    ticks: u64,
    at: Instant,
}

fn samples() -> &'static Mutex<HashMap<u32, Sample>> {
    static S: OnceLock<Mutex<HashMap<u32, Sample>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 根据前后两次 CPU 时间片差值计算占比（整机百分比）。
fn compute_cpu(pid: u32, ticks: u64, hz: f64, ncpu: usize) -> f32 {
    let now = Instant::now();
    let mut map = match samples().lock() {
        Ok(m) => m,
        Err(_) => return 0.0,
    };
    let cpu = match map.get(&pid) {
        Some(prev) => {
            let dt = now.duration_since(prev.at).as_secs_f64();
            let dticks = ticks.saturating_sub(prev.ticks);
            if dt > 0.0005 {
                (dticks as f64 / hz / dt * 100.0 / ncpu.max(1) as f64) as f32
            } else {
                0.0
            }
        }
        None => 0.0,
    };
    map.insert(pid, Sample { ticks, at: now });
    cpu
}

/// 清理已退出进程的采样，避免 map 无限增长。
fn prune(active: &HashSet<u32>) {
    if let Ok(mut map) = samples().lock() {
        map.retain(|pid, _| active.contains(pid));
    }
}

// ===========================================================================
// 公共 API
// ===========================================================================

/// 枚举当前系统内所有可读进程。
pub fn list_processes() -> Vec<ProcessInfo> {
    platform::list()
}

/// 逻辑 CPU 核数。
pub fn cpu_count() -> usize {
    platform::cpu_count()
}

/// 调整进程优先级。
pub fn set_priority(pid: u32, priority: Priority) -> Result<(), String> {
    platform::set_priority(pid, priority)
}

/// 温和终止进程（Unix 先 SIGTERM；Windows 调 TerminateProcess）。
pub fn terminate(pid: u32) -> Result<(), String> {
    platform::terminate(pid)
}

/// 判断当前是否拥有管理员 / root 权限。
pub fn is_elevated() -> bool {
    platform::is_elevated()
}

/// 判断进程是否仍然存在。
#[allow(dead_code)]
pub fn pid_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    }
    #[cfg(windows)]
    {
        platform::pid_exists(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
    {
        false
    }
}

// ===========================================================================
// Linux / Termux 实现
// ===========================================================================

#[cfg(any(target_os = "linux", target_os = "android"))]
mod platform {
    use super::*;

    /// 读取系统时钟频率（每秒 tick 数，通常 100）。
    fn clock_ticks() -> f64 {
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if hz > 0 {
            hz as f64
        } else {
            100.0
        }
    }

    fn page_size() -> u64 {
        let ps = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if ps > 0 {
            ps as u64
        } else {
            4096
        }
    }

    /// 从 `/proc/<pid>/stat` 解析 (进程名, utime+stime ticks)。
    fn read_stat(pid: u32) -> Option<(String, u64)> {
        let data = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
        // comm 可能包含空格与括号，取最后一对括号之间的内容。
        let open = data.find('(')?;
        let close = data.rfind(')')?;
        if close <= open {
            return None;
        }
        let name = data[open + 1..close].to_string();
        let rest: Vec<&str> = data[close + 1..].split_whitespace().collect();
        // rest[0] 是 state（字段 3），字段 14 = utime -> 下标 11，字段 15 = stime -> 下标 12。
        let utime: u64 = rest.get(11).and_then(|s| s.parse().ok()).unwrap_or(0);
        let stime: u64 = rest.get(12).and_then(|s| s.parse().ok()).unwrap_or(0);
        Some((name, utime + stime))
    }

    /// 从 `/proc/<pid>/statm` 读取常驻内存字节数。
    fn read_rss_bytes(pid: u32) -> u64 {
        if let Ok(data) = std::fs::read_to_string(format!("/proc/{}/statm", pid)) {
            if let Some(resident) = data.split_whitespace().nth(1) {
                if let Ok(pages) = resident.parse::<u64>() {
                    return pages * page_size();
                }
            }
        }
        0
    }

    /// 从 `/proc/<pid>/status` 读取真实 UID。
    fn read_uid(pid: u32) -> Option<u32> {
        let data = std::fs::read_to_string(format!("/proc/{}/status", pid)).ok()?;
        for line in data.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                let uid = rest.split_whitespace().next()?;
                return uid.parse().ok();
            }
        }
        None
    }

    /// 缓存 `/etc/passwd`，把 UID 解析成用户名。
    fn resolve_user(uid: u32) -> String {
        fn passwd_map() -> &'static HashMap<u32, String> {
            static M: OnceLock<HashMap<u32, String>> = OnceLock::new();
            M.get_or_init(|| {
                let mut map = HashMap::new();
                if let Ok(text) = std::fs::read_to_string("/etc/passwd") {
                    for line in text.lines() {
                        let parts: Vec<&str> = line.split(':').collect();
                        if parts.len() >= 3 {
                            if let Ok(id) = parts[2].parse::<u32>() {
                                map.insert(id, parts[0].to_string());
                            }
                        }
                    }
                }
                map
            })
        }
        passwd_map()
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string())
    }

    fn read_exe(pid: u32) -> String {
        std::fs::read_link(format!("/proc/{}/exe", pid))
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "-".to_string())
    }

    pub fn list() -> Vec<ProcessInfo> {
        let hz = clock_ticks();
        let ncpu = cpu_count();
        let mut out = Vec::new();
        let mut active = HashSet::new();

        let entries = match std::fs::read_dir("/proc") {
            Ok(e) => e,
            Err(_) => return out,
        };

        for entry in entries.flatten() {
            let pid = match entry.file_name().to_string_lossy().parse::<u32>() {
                Ok(p) => p,
                Err(_) => continue,
            };
            active.insert(pid);
            let (name, ticks) = match read_stat(pid) {
                Some(v) => v,
                None => continue,
            };
            let uid = read_uid(pid);
            let user = match uid {
                Some(u) => resolve_user(u),
                None => "-".to_string(),
            };
            let is_service = uid == Some(0);
            let cpu = if cpu_count() == 0 {
                0.0
            } else {
                compute_cpu(pid, ticks, hz, ncpu)
            };
            let memory_mb = read_rss_bytes(pid) as f64 / 1024.0 / 1024.0;
            out.push(ProcessInfo {
                pid,
                name,
                exe: read_exe(pid),
                user,
                cpu_percent: cpu,
                memory_mb,
                is_service,
            });
        }

        prune(&active);
        out.sort_by(|a, b| {
            b.cpu_percent
                .partial_cmp(&a.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }

    pub fn cpu_count() -> usize {
        let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
        if n > 0 {
            n as usize
        } else {
            1
        }
    }

    pub fn set_priority(pid: u32, priority: Priority) -> Result<(), String> {
        let nice = priority.to_nice();
        let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid, nice) };
        if rc == 0 {
            Ok(())
        } else {
            let err = std::io::Error::last_os_error();
            Err(format!(
                "调整 nice 失败（{}）；提高优先级通常需要 root，可尝试 sudo。",
                err
            ))
        }
    }

    pub fn terminate(pid: u32) -> Result<(), String> {
        // 先温和 SIGTERM，给进程自行清理的机会。
        let rc = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        if rc == 0 {
            Ok(())
        } else {
            Err(format!(
                "发送 SIGTERM 失败：{}",
                std::io::Error::last_os_error()
            ))
        }
    }

    pub fn is_elevated() -> bool {
        unsafe { libc::geteuid() == 0 }
    }
}

// ===========================================================================
// Windows 实现
// ===========================================================================

#[cfg(windows)]
mod platform {
    use super::*;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, SetPriorityClass,
        TerminateProcess,
    };

    const PROCESS_TERMINATE: u32 = 0x0001;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const PROCESS_VM_READ: u32 = 0x0010;
    const PROCESS_SET_INFORMATION: u32 = 0x0200;

    const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
    const NORMAL_PRIORITY_CLASS: u32 = 0x0000_0020;
    const ABOVE_NORMAL_PRIORITY_CLASS: u32 = 0x0000_8000;
    const HIGH_PRIORITY_CLASS: u32 = 0x0000_0080;

    fn wide_to_string(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    /// Windows 服务运行在 Session 0；以此作为“服务进程”的简化判定。
    fn is_service_process(pid: u32) -> bool {
        let mut session: u32 = 0;
        let rc = unsafe { ProcessIdToSessionId(pid, &mut session) };
        rc != 0 && session == 0
    }

    fn open_for_query(pid: u32) -> HANDLE {
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid) };
        if h.is_null() {
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) }
        } else {
            h
        }
    }

    fn tick_to_u64(ft: FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
    }

    fn filetime_to_millis(ft: FILETIME) -> u64 {
        tick_to_u64(ft) / 10_000
    }

    pub fn list() -> Vec<ProcessInfo> {
        let mut out = Vec::new();
        let ncpu = cpu_count() as u64;
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
            return out;
        }

        let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) };
        let mut active = HashSet::new();
        while ok != 0 {
            let pid = entry.th32ProcessID;
            if pid != 0 {
                let name = wide_to_string(&entry.szExeFile);
                active.insert(pid);

                let mut exe = String::from("-");
                let mut memory_mb = 0.0f64;
                let mut cpu = 0.0f32;

                let handle = open_for_query(pid);
                if !handle.is_null() {
                    // 可执行路径
                    let mut buf = vec![0u16; 260];
                    let mut size = buf.len() as u32;
                    if unsafe { QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size) }
                        != 0
                    {
                        exe = wide_to_string(&buf[..size as usize]);
                    }
                    // 内存
                    let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
                    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
                    if unsafe { GetProcessMemoryInfo(handle, &mut counters, counters.cb) } != 0 {
                        memory_mb = counters.WorkingSetSize as f64 / 1024.0 / 1024.0;
                    }
                    // CPU 时间（内核 + 用户），单位 100ns
                    let mut creation: FILETIME = unsafe { std::mem::zeroed() };
                    let mut exit_time: FILETIME = unsafe { std::mem::zeroed() };
                    let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
                    let mut user: FILETIME = unsafe { std::mem::zeroed() };
                    if unsafe {
                        GetProcessTimes(
                            handle,
                            &mut creation,
                            &mut exit_time,
                            &mut kernel,
                            &mut user,
                        )
                    } != 0
                    {
                        let total = filetime_to_millis(kernel) + filetime_to_millis(user);
                        cpu = compute_cpu(pid, total, 1000.0, ncpu.max(1) as usize);
                    }
                    unsafe { CloseHandle(handle) };
                }

                out.push(ProcessInfo {
                    pid,
                    name,
                    exe,
                    user: "-".to_string(),
                    cpu_percent: cpu,
                    memory_mb,
                    is_service: is_service_process(pid),
                });
            }
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            ok = unsafe { Process32NextW(snapshot, &mut entry) };
        }
        unsafe { CloseHandle(snapshot) };
        prune(&active);
        out.sort_by(|a, b| {
            b.cpu_percent
                .partial_cmp(&a.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }

    pub fn cpu_count() -> usize {
        let mut info: SYSTEM_INFO = unsafe { std::mem::zeroed() };
        unsafe { GetSystemInfo(&mut info) };
        let n = info.dwNumberOfProcessors as usize;
        if n > 0 {
            n
        } else {
            1
        }
    }

    pub fn set_priority(pid: u32, priority: Priority) -> Result<(), String> {
        let class = match priority {
            Priority::High => HIGH_PRIORITY_CLASS,
            Priority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
            Priority::Normal => NORMAL_PRIORITY_CLASS,
            Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
            Priority::Idle => IDLE_PRIORITY_CLASS,
        };
        let handle = unsafe { OpenProcess(PROCESS_SET_INFORMATION, 0, pid) };
        if handle.is_null() {
            return Err("打开进程失败，可能需要以管理员身份运行。".to_string());
        }
        let rc = unsafe { SetPriorityClass(handle, class) };
        unsafe { CloseHandle(handle) };
        if rc != 0 {
            Ok(())
        } else {
            Err("设置进程优先级失败，可能需要管理员权限。".to_string())
        }
    }

    pub fn terminate(pid: u32) -> Result<(), String> {
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if handle.is_null() {
            return Err("打开进程失败，可能需要管理员权限。".to_string());
        }
        let rc = unsafe { TerminateProcess(handle, 1) };
        unsafe { CloseHandle(handle) };
        if rc != 0 {
            Ok(())
        } else {
            Err("终止进程失败。".to_string())
        }
    }

    pub fn pid_exists(pid: u32) -> bool {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            false
        } else {
            unsafe { CloseHandle(handle) };
            true
        }
    }

    pub fn is_elevated() -> bool {
        // 简化判断：尝试打开系统进程，成功即认为有较高权限。
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, 4) };
        if handle.is_null() {
            false
        } else {
            unsafe { CloseHandle(handle) };
            true
        }
    }
}

// ===========================================================================
// 其它平台（功能降级）
// ===========================================================================

#[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
mod platform {
    use super::*;

    pub fn list() -> Vec<ProcessInfo> {
        Vec::new()
    }

    pub fn cpu_count() -> usize {
        1
    }

    pub fn set_priority(_pid: u32, _priority: Priority) -> Result<(), String> {
        Err("当前平台暂不支持调整优先级。".to_string())
    }

    pub fn terminate(_pid: u32) -> Result<(), String> {
        Err("当前平台暂不支持终止进程。".to_string())
    }

    pub fn is_elevated() -> bool {
        false
    }
}
