//! 规则引擎与守护循环。
//!
//! 单次 `tick()` 的工作流：
//!   1. 枚举进程；
//!   2. 过滤黑名单 / 自身；
//!   3. 按规则顺序做**首个命中**匹配（避免同一进程被多个分组争抢）；
//!   4. 施加 CPU 上限（cgroup / Job Object / 信号回退）；
//!   5. 检查内存硬阈值并执行策略；
//!   6. 绑定进程优先级。
//!
//! 守护循环只在用户显式开启时运行；退出或被杀时 `release_all` 会撤销全部限制。

use crate::core::model::{MemoryAction, Priority, ProcessInfo, Rule, RuleSet};
use crate::platform::limiter::CpuLimiter;
use crate::platform::{process, temperature};
use crate::utils::logger;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 全局关机标志：由信号处理器在收到 Ctrl+C / SIGTERM 时置位。
/// 使用原子布尔值，信号处理器内是异步信号安全的。
pub static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// 守护运行参数。
#[derive(Clone)]
pub struct EngineConfig {
    /// 轮询间隔（秒），最小 1 秒。
    pub interval_secs: u64,
    /// 是否启用温度联动。
    pub temperature_enabled: bool,
    /// 试运行：只记录将要执行的动作，不做任何实际修改。
    pub dry_run: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            interval_secs: 2,
            temperature_enabled: false,
            dry_run: false,
        }
    }
}

/// 日志节流：同一 key 在冷却时间内只记录一次，避免刷屏。
struct Cooldown {
    window: Duration,
    last: HashMap<String, Instant>,
}

impl Cooldown {
    fn new(secs: u64) -> Self {
        Cooldown {
            window: Duration::from_secs(secs),
            last: HashMap::new(),
        }
    }

    fn allow(&mut self, key: &str) -> bool {
        let now = Instant::now();
        match self.last.get(key) {
            Some(t) if now.duration_since(*t) < self.window => false,
            _ => {
                self.last.insert(key.to_string(), now);
                true
            }
        }
    }

    fn prune(&mut self, max_entries: usize) {
        if self.last.len() > max_entries {
            let now = Instant::now();
            self.last
                .retain(|_, t| now.duration_since(*t) < self.window);
        }
    }
}

/// 资源守护引擎。
pub struct Engine {
    pub config: EngineConfig,
    pub ruleset: RuleSet,
    limiter: CpuLimiter,
    applied_priority: HashMap<u32, Priority>,
    terminated: HashSet<u32>,
    memory_cooldown: Cooldown,
    temp_hot: bool,
    /// 最近一次 tick 观察到的进程数，供状态展示。
    pub last_process_count: usize,
    pub last_tick: Option<Instant>,
}

impl Engine {
    pub fn new(ruleset: RuleSet, config: EngineConfig) -> Self {
        Engine {
            config,
            ruleset,
            limiter: CpuLimiter::new(),
            applied_priority: HashMap::new(),
            terminated: HashSet::new(),
            memory_cooldown: Cooldown::new(60),
            temp_hot: false,
            last_process_count: 0,
            last_tick: None,
        }
    }

    /// 当前 CPU 限速后端名称。
    pub fn backend(&self) -> &'static str {
        self.limiter.backend()
    }

    /// 计算规则在给定温度下的有效 CPU 上限（占整机百分比）。
    fn effective_cpu(&self, rule: &Rule, temp: Option<f32>) -> Option<f32> {
        let base = rule.cpu_limit_percent;
        if let (Some(link), Some(t)) = (rule.temperature_link.as_ref(), temp) {
            if t >= link.threshold_c {
                return Some(match base {
                    Some(b) => b.min(link.reduced_cpu_percent),
                    None => link.reduced_cpu_percent,
                });
            }
        }
        base
    }

    /// 执行一轮检查，返回本轮产生的动作条数。
    pub fn tick(&mut self) -> usize {
        let _self_pid = std::process::id();
        let ncpu = process::cpu_count();
        let unlimited = 100.0 * ncpu.max(1) as f32;
        let processes = process::list_processes();
        self.last_process_count = processes.len();
        self.last_tick = Some(Instant::now());

        let temp = if self.config.temperature_enabled {
            temperature::read_celsius()
        } else {
            None
        };

        // 温度联动状态变化提示（仅记录一次）。
        if let Some(t) = temp {
            let hot = self
                .ruleset
                .rules
                .iter()
                .filter_map(|r| r.temperature_link.as_ref())
                .any(|l| t >= l.threshold_c);
            if hot != self.temp_hot {
                if hot {
                    logger::log_action(
                        "TEMP_HIGH",
                        "",
                        0,
                        &format!("当前 {:.1}°C，已进一步压低 CPU 上限", t),
                    );
                } else {
                    logger::log_action("TEMP_OK", "", 0, &format!("当前 {:.1}°C，恢复正常上限", t));
                }
                self.temp_hot = hot;
            }
        }

        // 克隆规则与黑名单，避免在调用 &mut self 方法时与不可变借用冲突。
        let rules = self.ruleset.rules.clone();
        let blacklist = self.ruleset.blacklist.clone();

        // 首轮匹配：每个进程只归属第一条命中的规则。
        let mut groups: HashMap<String, Vec<u32>> = HashMap::new();
        let mut actions = 0usize;

        for p in &processes {
            if p.pid == _self_pid
                || blacklist
                    .iter()
                    .any(|pat| crate::core::model::glob_match(pat, &p.name))
            {
                continue;
            }
            for rule in &rules {
                if !rule.enabled || !rule.matches_name(&p.name) {
                    continue;
                }
                groups.entry(rule.id.clone()).or_default().push(p.pid);
                actions += self.apply_memory(rule, p);
                actions += self.apply_priority(rule, p);
                break;
            }
        }

        // CPU 限制：对所有启用规则调用 enforce；没有 CPU 上限的规则使用“无限制”。
        for rule in &rules {
            if !rule.enabled {
                continue;
            }
            let pids = groups.get(&rule.id).cloned().unwrap_or_default();
            let percent = self.effective_cpu(rule, temp).unwrap_or(unlimited);
            if self.config.dry_run {
                continue;
            }
            if let Err(e) = self.limiter.enforce(&rule.id, percent, ncpu, &pids) {
                if self.memory_cooldown.allow(&format!("cpuerr:{}", rule.id)) {
                    logger::log_warn(&format!("施加 CPU 限制失败：{}", e));
                }
            }
        }

        // 清理已退出进程的状态。
        let alive: HashSet<u32> = processes.iter().map(|p| p.pid).collect();
        self.applied_priority.retain(|pid, _| alive.contains(pid));
        self.terminated.retain(|pid| alive.contains(pid));
        self.memory_cooldown.prune(4096);

        actions
    }

    /// 内存硬阈值检查与处置。
    fn apply_memory(&mut self, rule: &Rule, p: &ProcessInfo) -> usize {
        let limit = match rule.memory_limit_mb {
            Some(l) if l > 0 => l,
            _ => return 0,
        };
        if p.memory_mb <= limit as f64 {
            return 0;
        }
        let key = format!("mem:{}:{}", rule.id, p.pid);
        let detail = format!(
            "{:.1} MiB 超过阈值 {} MiB（动作：{}）",
            p.memory_mb,
            limit,
            rule.memory_action.label()
        );

        match rule.memory_action {
            MemoryAction::Warn | MemoryAction::GcHint => {
                if self.memory_cooldown.allow(&key) {
                    let action = if rule.memory_action == MemoryAction::GcHint {
                        "MEM_GC_HINT"
                    } else {
                        "MEM_WARN"
                    };
                    let suffix = if rule.memory_action == MemoryAction::GcHint {
                        "；建议进程自身触发 GC 释放内存"
                    } else {
                        ""
                    };
                    logger::log_action(action, &p.name, p.pid, &format!("{}{}", detail, suffix));
                    return 1;
                }
                0
            }
            MemoryAction::LowerPriority => {
                if self.applied_priority.get(&p.pid) != Some(&Priority::BelowNormal) {
                    if !self.config.dry_run {
                        let _ = process::set_priority(p.pid, Priority::BelowNormal);
                    }
                    self.applied_priority.insert(p.pid, Priority::BelowNormal);
                    logger::log_action("MEM_LOWER_PRIO", &p.name, p.pid, &detail);
                    return 1;
                }
                0
            }
            MemoryAction::Terminate => {
                if !self.terminated.contains(&p.pid) && self.memory_cooldown.allow(&key) {
                    if !self.config.dry_run {
                        match process::terminate(p.pid) {
                            Ok(()) => logger::log_action(
                                "MEM_TERMINATE",
                                &p.name,
                                p.pid,
                                &format!("{}；已发送温和终止信号", detail),
                            ),
                            Err(e) => logger::log_warn(&format!(
                                "终止 {}（PID {}）失败：{}",
                                p.name, p.pid, e
                            )),
                        }
                    }
                    self.terminated.insert(p.pid);
                    return 1;
                }
                0
            }
        }
    }

    /// 规则绑定的优先级调整（每个进程只施加一次）。
    fn apply_priority(&mut self, rule: &Rule, p: &ProcessInfo) -> usize {
        let priority = match rule.priority {
            Some(pr) => pr,
            None => return 0,
        };
        if self.applied_priority.get(&p.pid) == Some(&priority) {
            return 0;
        }
        if !self.config.dry_run {
            match process::set_priority(p.pid, priority) {
                Ok(()) => logger::log_action(
                    "PRIORITY",
                    &p.name,
                    p.pid,
                    &format!("优先级设为 {}", priority.label()),
                ),
                Err(e) => {
                    if self.memory_cooldown.allow(&format!("prioerr:{}", p.pid)) {
                        logger::log_warn(&format!("调整 {} 优先级失败：{}", p.name, e));
                    }
                    return 0;
                }
            }
        }
        self.applied_priority.insert(p.pid, priority);
        1
    }

    /// 守护主循环，直到 `running` 变为 false。
    pub fn run(&mut self, running: Arc<AtomicBool>) {
        let interval = self.config.interval_secs.max(1);
        logger::log_action(
            "GUARD_START",
            "",
            0,
            &format!(
                "间隔 {}s，后端 {}，温度联动 {}，规则集「{}」",
                interval,
                self.backend(),
                if self.config.temperature_enabled {
                    "开"
                } else {
                    "关"
                },
                self.ruleset.name
            ),
        );

        while running.load(Ordering::Relaxed) && !SHUTDOWN.load(Ordering::Relaxed) {
            let _ = self.tick();
            // 以 100ms 粒度睡眠，保证能及时响应停止请求。
            let mut elapsed = 0u64;
            while elapsed < interval * 10
                && running.load(Ordering::Relaxed)
                && !SHUTDOWN.load(Ordering::Relaxed)
            {
                std::thread::sleep(Duration::from_millis(100));
                elapsed += 1;
            }
        }

        self.release_all();
        logger::log_action("GUARD_STOP", "", 0, "守护已停止，限制已全部释放");
    }

    /// 释放全部限制并清空内部状态。
    pub fn release_all(&mut self) {
        let _ = self.limiter.release_all();
        self.applied_priority.clear();
        self.terminated.clear();
    }
}

/// 紧急清除：不需要已有 Engine 实例，直接尝试释放所有已施加的限制。
pub fn emergency_clear() {
    let mut limiter = CpuLimiter::new();
    let _ = limiter.release_all();
    logger::log_action("EMERGENCY_CLEAR", "", 0, "已尝试清除全部 CPU 限制");
}
