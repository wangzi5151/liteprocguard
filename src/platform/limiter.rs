//! CPU 限速后端。
//!
//! 优先级从高到低：
//!   1. **Linux cgroup v2** —— 写入 `cpu.max`，最精确、零额外开销；
//!   2. **Linux cgroup v1** —— 写入 `cpu.cfs_quota_us`；
//!   3. **信号回退（SIGSTOP/SIGCONT 占空比）** —— cgroup 不可用（无 root /
//!      Android 内核裁剪）时的兼容方案，**不是杀进程**，而是按比例暂停/恢复；
//!   4. **Windows Job Object** —— `JOB_OBJECT_CPU_RATE_CONTROL` 硬上限。
//!
//! 所有后端都遵循“可回滚”原则：`release_all` 会把进程移回原始状态。

/// CPU 限制器统一门面。
pub struct CpuLimiter {
    inner: imp::Inner,
}

impl CpuLimiter {
    pub fn new() -> Self {
        CpuLimiter {
            inner: imp::Inner::new(),
        }
    }

    /// 探测本机可用的 CPU 限速后端名称（无副作用，供状态展示）。
    pub fn detect_backend() -> &'static str {
        imp::detect_backend()
    }

    /// 当前使用的后端名称（用于提示用户）。
    pub fn backend(&self) -> &'static str {
        self.inner.backend()
    }

    /// 后端是否可用（信号回退恒可用）。
    #[allow(dead_code)]
    pub fn available(&self) -> bool {
        self.inner.available()
    }

    /// 对某条规则对应的进程集合施加 CPU 上限。
    ///
    /// * `key`     —— 规则分组名（建议用规则 ID）
    /// * `percent` —— CPU 上限，占整机百分比（0-100）
    /// * `ncpu`    —— 逻辑核数
    /// * `pids`    —— 当前命中且需要限制的 PID 列表
    pub fn enforce(
        &mut self,
        key: &str,
        percent: f32,
        ncpu: usize,
        pids: &[u32],
    ) -> Result<(), String> {
        self.inner.enforce(key, percent, ncpu, pids)
    }

    /// 释放所有限制，恢复系统原生状态。
    pub fn release_all(&mut self) -> Result<(), String> {
        self.inner.release_all()
    }
}

impl Default for CpuLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for CpuLimiter {
    fn drop(&mut self) {
        // 进程异常退出时尽最大努力撤销限制，避免锁死系统。
        let _ = self.inner.release_all();
    }
}

/// 把规则键转换为安全的目录名。
#[allow(dead_code)]
fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

// ===========================================================================
// Linux / Termux
// ===========================================================================

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[allow(dead_code)]
    enum Mode {
        Cgroup(CgroupState),
        Signal(SignalThrottle),
        None,
    }

    pub struct Inner {
        mode: Mode,
        assigned: HashMap<u32, String>,
        groups: HashMap<String, HashSet<u32>>,
    }

    // ------------------------- cgroup -------------------------

    struct CgroupState {
        v2: bool,
        base: PathBuf,
    }

    impl CgroupState {
        fn detect() -> Option<CgroupState> {
            let v2root = PathBuf::from("/sys/fs/cgroup");
            if v2root.join("cgroup.controllers").exists() {
                return Some(CgroupState {
                    v2: true,
                    base: v2root.join("liteprocguard"),
                });
            }
            let v1root = PathBuf::from("/sys/fs/cgroup/cpu");
            if v1root.is_dir() {
                return Some(CgroupState {
                    v2: false,
                    base: v1root.join("liteprocguard"),
                });
            }
            None
        }

        /// 判断是否有权限创建/写入 cgroup。
        fn writable(&self) -> bool {
            if std::fs::create_dir_all(&self.base).is_err() {
                return false;
            }
            if self.v2 {
                let _ = std::fs::write("/sys/fs/cgroup/cgroup.subtree_control", "+cpu");
                let _ = std::fs::write(self.base.join("cgroup.subtree_control"), "+cpu");
            }
            true
        }

        fn group_path(&self, key: &str) -> PathBuf {
            self.base.join(sanitize_key(key))
        }

        fn ensure_group(&self, key: &str) -> Result<PathBuf, String> {
            let path = self.group_path(key);
            std::fs::create_dir_all(&path).map_err(|e| {
                format!(
                    "创建 cgroup 目录失败（{}）；该功能通常需要 root：sudo liteprocguard ...",
                    e
                )
            })?;
            if self.v2 {
                let _ = std::fs::write(self.base.join("cgroup.subtree_control"), "+cpu");
            }
            Ok(path)
        }

        fn set_limit(&self, key: &str, percent: f32, ncpu: usize) -> Result<(), String> {
            let path = self.group_path(key);
            const PERIOD: u64 = 100_000;
            let unlimited = percent <= 0.0 || percent >= 100.0 * ncpu.max(1) as f32;
            if self.v2 {
                let value = if unlimited {
                    "max".to_string()
                } else {
                    let quota =
                        (PERIOD as f64 * (percent as f64 / 100.0) * ncpu as f64).max(1000.0) as u64;
                    format!("{} {}", quota, PERIOD)
                };
                std::fs::write(path.join("cpu.max"), value)
                    .map_err(|e| format!("写入 cpu.max 失败（{}）；需要 root 权限。", e))?;
            } else {
                let _ = std::fs::write(path.join("cpu.cfs_period_us"), PERIOD.to_string());
                let value = if unlimited {
                    "-1".to_string()
                } else {
                    let quota =
                        (PERIOD as f64 * (percent as f64 / 100.0) * ncpu as f64).max(1000.0) as i64;
                    quota.to_string()
                };
                std::fs::write(path.join("cpu.cfs_quota_us"), value).map_err(|e| {
                    format!("写入 cpu.cfs_quota_us 失败（{}）；需要 root 权限。", e)
                })?;
            }
            Ok(())
        }

        fn assign(&self, key: &str, pid: u32) -> Result<(), String> {
            let path = self.group_path(key);
            let file = if self.v2 {
                path.join("cgroup.procs")
            } else {
                path.join("tasks")
            };
            std::fs::write(&file, pid.to_string())
                .map_err(|e| format!("将 PID {} 加入 cgroup 失败（{}）；请以 root 运行。", pid, e))
        }

        fn unassign(&self, pid: u32) {
            let file = if self.v2 {
                PathBuf::from("/sys/fs/cgroup/cgroup.procs")
            } else {
                PathBuf::from("/sys/fs/cgroup/cpu/tasks")
            };
            let _ = std::fs::write(file, pid.to_string());
        }

        fn pids_in(&self, path: &Path) -> Vec<u32> {
            let file = if self.v2 {
                path.join("cgroup.procs")
            } else {
                path.join("tasks")
            };
            std::fs::read_to_string(file)
                .map(|t| {
                    t.lines()
                        .filter_map(|l| l.trim().parse::<u32>().ok())
                        .collect()
                })
                .unwrap_or_default()
        }

        fn release_all(&self) {
            if let Ok(entries) = std::fs::read_dir(&self.base) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        for pid in self.pids_in(&path) {
                            self.unassign(pid);
                        }
                        let _ = std::fs::remove_dir(&path);
                    }
                }
            }
            let _ = std::fs::remove_dir(&self.base);
        }
    }

    // ------------------------- 信号回退 -------------------------

    struct SignalThrottle {
        ratios: Arc<Mutex<HashMap<u32, f32>>>,
        stop: Arc<AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl SignalThrottle {
        fn new() -> Self {
            const TICK_MS: u64 = 50;
            const SLOTS: u64 = 20;
            let ratios: Arc<Mutex<HashMap<u32, f32>>> = Arc::new(Mutex::new(HashMap::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let r = ratios.clone();
            let s = stop.clone();
            let handle = std::thread::Builder::new()
                .name("lpg-signal".to_string())
                .spawn(move || {
                    let mut stopped: HashSet<u32> = HashSet::new();
                    let mut tick: u64 = 0;
                    while !s.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(TICK_MS));
                        tick = tick.wrapping_add(1);
                        let map = match r.lock() {
                            Ok(m) => m.clone(),
                            Err(_) => continue,
                        };
                        // 恢复已不再受限的进程。
                        for &pid in stopped.iter() {
                            if !map.contains_key(&pid) {
                                unsafe {
                                    libc::kill(pid as i32, libc::SIGCONT);
                                }
                            }
                        }
                        stopped.retain(|p| map.contains_key(p));
                        for (&pid, &ratio) in map.iter() {
                            if pid <= 2 {
                                continue;
                            }
                            if ratio >= 99.0 {
                                if stopped.remove(&pid) {
                                    unsafe {
                                        libc::kill(pid as i32, libc::SIGCONT);
                                    }
                                }
                                continue;
                            }
                            let slots_on =
                                (((ratio / 100.0) * SLOTS as f32).round() as u64).clamp(1, SLOTS);
                            let on = (tick % SLOTS) < slots_on;
                            if on {
                                if stopped.remove(&pid) {
                                    unsafe {
                                        libc::kill(pid as i32, libc::SIGCONT);
                                    }
                                }
                            } else if stopped.insert(pid) {
                                unsafe {
                                    libc::kill(pid as i32, libc::SIGSTOP);
                                }
                            }
                        }
                    }
                    for pid in stopped {
                        unsafe {
                            libc::kill(pid as i32, libc::SIGCONT);
                        }
                    }
                })
                .ok();
            SignalThrottle {
                ratios,
                stop,
                handle,
            }
        }

        fn set(&self, pid: u32, percent: f32) {
            if let Ok(mut m) = self.ratios.lock() {
                m.insert(pid, percent);
            }
        }

        fn remove(&self, pid: u32) {
            if let Ok(mut m) = self.ratios.lock() {
                m.remove(&pid);
            }
        }

        fn clear(&self) {
            if let Ok(mut m) = self.ratios.lock() {
                m.clear();
            }
        }
    }

    impl Drop for SignalThrottle {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.clear();
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    // ------------------------- Inner -------------------------

    pub fn detect_backend() -> &'static str {
        match CgroupState::detect() {
            Some(c) => {
                let root = unsafe { libc::geteuid() } == 0;
                match (c.v2, root) {
                    (true, true) => "cgroup v2",
                    (false, true) => "cgroup v1",
                    _ => "信号回退（需 root 才能使用 cgroup）",
                }
            }
            None => "信号回退（SIGSTOP/SIGCONT 占空比）",
        }
    }

    impl Inner {
        pub fn new() -> Self {
            let mode = match CgroupState::detect() {
                Some(cg) if cg.writable() => Mode::Cgroup(cg),
                _ => Mode::Signal(SignalThrottle::new()),
            };
            Inner {
                mode,
                assigned: HashMap::new(),
                groups: HashMap::new(),
            }
        }

        pub fn backend(&self) -> &'static str {
            match &self.mode {
                Mode::Cgroup(c) if c.v2 => "cgroup v2",
                Mode::Cgroup(_) => "cgroup v1",
                Mode::Signal(_) => "信号回退（SIGSTOP/SIGCONT 占空比）",
                Mode::None => "无",
            }
        }

        #[allow(dead_code)]
        pub fn available(&self) -> bool {
            !matches!(self.mode, Mode::None)
        }

        pub fn enforce(
            &mut self,
            key: &str,
            percent: f32,
            ncpu: usize,
            pids: &[u32],
        ) -> Result<(), String> {
            let set: HashSet<u32> = pids.iter().copied().collect();

            // 1) 把离开本分组的进程恢复。
            if let Some(group) = self.groups.get(key) {
                let stale: Vec<u32> = group.iter().copied().filter(|p| !set.contains(p)).collect();
                for pid in stale {
                    if self.assigned.get(&pid).map(|k| k == key).unwrap_or(false) {
                        match &self.mode {
                            Mode::Cgroup(c) => c.unassign(pid),
                            Mode::Signal(s) => s.remove(pid),
                            Mode::None => {}
                        }
                        self.assigned.remove(&pid);
                    }
                }
            }

            // 2) 施加限制并加入新进程。
            match &self.mode {
                Mode::Cgroup(c) => {
                    c.ensure_group(key)?;
                    c.set_limit(key, percent, ncpu)?;
                    for &pid in &set {
                        if self.assigned.get(&pid).map(|k| k == key).unwrap_or(false) {
                            continue;
                        }
                        c.assign(key, pid)?;
                        self.assigned.insert(pid, key.to_string());
                    }
                }
                Mode::Signal(s) => {
                    for &pid in &set {
                        if self.assigned.get(&pid).map(|k| k == key).unwrap_or(false) {
                            continue;
                        }
                        s.set(pid, percent);
                        self.assigned.insert(pid, key.to_string());
                    }
                }
                Mode::None => return Err("当前系统没有可用的 CPU 限制后端。".to_string()),
            }

            self.groups.insert(key.to_string(), set);
            Ok(())
        }

        pub fn release_all(&mut self) -> Result<(), String> {
            match &self.mode {
                Mode::Cgroup(c) => c.release_all(),
                Mode::Signal(s) => s.clear(),
                Mode::None => {}
            }
            self.assigned.clear();
            self.groups.clear();
            Ok(())
        }
    }
}

// ===========================================================================
// Windows Job Object
// ===========================================================================

#[cfg(windows)]
mod imp {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectCpuRateControlInformation,
        SetInformationJobObject, JOBOBJECT_CPU_RATE_CONTROL_INFORMATION,
        JOB_OBJECT_CPU_RATE_CONTROL_ENABLE, JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
    };
    use windows_sys::Win32::System::Threading::OpenProcess;

    const PROCESS_SET_QUOTA: u32 = 0x0100;
    const PROCESS_TERMINATE: u32 = 0x0001;

    pub struct Inner {
        jobs: HashMap<String, HANDLE>,
        assigned: HashMap<u32, String>,
        groups: HashMap<String, HashSet<u32>>,
    }

    pub fn detect_backend() -> &'static str {
        "Windows Job Object"
    }

    impl Inner {
        pub fn new() -> Self {
            Inner {
                jobs: HashMap::new(),
                assigned: HashMap::new(),
                groups: HashMap::new(),
            }
        }

        pub fn backend(&self) -> &'static str {
            "Windows Job Object"
        }

        #[allow(dead_code)]
        pub fn available(&self) -> bool {
            true
        }

        fn ensure_job(&mut self, key: &str) -> HANDLE {
            if let Some(h) = self.jobs.get(key) {
                return *h;
            }
            let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            self.jobs.insert(key.to_string(), h);
            h
        }

        fn set_rate(&self, key: &str, percent: f32, ncpu: usize) {
            if let Some(h) = self.jobs.get(key) {
                let mut info: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION =
                    unsafe { std::mem::zeroed() };
                let unlimited = percent <= 0.0 || percent >= 100.0 * ncpu.max(1) as f32;
                if unlimited {
                    info.ControlFlags = 0;
                    info.Anonymous.CpuRate = 0;
                } else {
                    info.ControlFlags =
                        JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP;
                    info.Anonymous.CpuRate = (percent * 100.0).round().clamp(1.0, 10000.0) as u32;
                }
                unsafe {
                    SetInformationJobObject(
                        *h,
                        JobObjectCpuRateControlInformation,
                        &info as *const _ as *const core::ffi::c_void,
                        std::mem::size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
                    );
                }
            }
        }

        fn assign(&mut self, key: &str, pid: u32) -> Result<(), String> {
            let h = self.ensure_job(key);
            if h.is_null() {
                return Err("创建 Job Object 失败。".to_string());
            }
            let ph = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
            if ph.is_null() {
                return Err(format!("打开进程 {} 失败（权限不足或进程受保护）。", pid));
            }
            let rc = unsafe { AssignProcessToJobObject(h, ph) };
            unsafe { CloseHandle(ph) };
            if rc != 0 {
                Ok(())
            } else {
                Err(format!(
                    "将 PID {} 加入 Job 失败（该进程可能已属于其它 Job）。",
                    pid
                ))
            }
        }

        pub fn enforce(
            &mut self,
            key: &str,
            percent: f32,
            ncpu: usize,
            pids: &[u32],
        ) -> Result<(), String> {
            self.ensure_job(key);
            self.set_rate(key, percent, ncpu);
            let set: HashSet<u32> = pids.iter().copied().collect();

            if let Some(group) = self.groups.get(key) {
                let stale: Vec<u32> = group.iter().copied().filter(|p| !set.contains(p)).collect();
                for pid in stale {
                    self.assigned.remove(&pid);
                }
            }

            for &pid in &set {
                if self.assigned.get(&pid).map(|k| k == key).unwrap_or(false) {
                    continue;
                }
                if let Err(e) = self.assign(key, pid) {
                    // Windows 无法把一个进程从 Job 中移除，这里只记录，不中断整体。
                    crate::utils::logger::log_warn(&e);
                    continue;
                }
                self.assigned.insert(pid, key.to_string());
            }

            self.groups.insert(key.to_string(), set);
            Ok(())
        }

        pub fn release_all(&mut self) -> Result<(), String> {
            for h in self.jobs.values() {
                let mut info: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION =
                    unsafe { std::mem::zeroed() };
                info.ControlFlags = 0;
                unsafe {
                    SetInformationJobObject(
                        *h,
                        JobObjectCpuRateControlInformation,
                        &info as *const _ as *const core::ffi::c_void,
                        std::mem::size_of::<JOBOBJECT_CPU_RATE_CONTROL_INFORMATION>() as u32,
                    );
                    CloseHandle(*h);
                }
            }
            self.jobs.clear();
            self.assigned.clear();
            self.groups.clear();
            Ok(())
        }
    }
}

// ===========================================================================
// 其它平台
// ===========================================================================

#[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
mod imp {
    pub struct Inner;

    pub fn detect_backend() -> &'static str {
        "无"
    }

    impl Inner {
        pub fn new() -> Self {
            Inner
        }
        pub fn backend(&self) -> &'static str {
            "无"
        }
        #[allow(dead_code)]
        pub fn available(&self) -> bool {
            false
        }
        pub fn enforce(
            &mut self,
            _key: &str,
            _percent: f32,
            _ncpu: usize,
            _pids: &[u32],
        ) -> Result<(), String> {
            Err("当前平台暂不支持 CPU 限速。".to_string())
        }
        pub fn release_all(&mut self) -> Result<(), String> {
            Ok(())
        }
    }
}
