//! 核心数据模型：规则、规则集、进程快照、动作枚举。
//!
//! 所有结构体都可直接序列化为 JSON，方便规则导入导出、预设分发。

use serde::{Deserialize, Serialize};

/// 内存超阈值后的处置策略。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAction {
    /// 只告警，不采取任何强制动作（“不杀只告警”模式）。
    #[default]
    Warn,
    /// 降低进程优先级（nice / 进程优先级），温和让出资源。
    LowerPriority,
    /// 提示进程（若适用）触发垃圾回收 —— 仅记录日志建议，不注入。
    GcHint,
    /// 温和终止：先 SIGTERM / TaskKill，不做 SIGKILL 式强杀。
    Terminate,
}

impl MemoryAction {
    pub fn label(&self) -> &'static str {
        match self {
            MemoryAction::Warn => "仅告警",
            MemoryAction::LowerPriority => "降低优先级",
            MemoryAction::GcHint => "GC 提示",
            MemoryAction::Terminate => "温和终止",
        }
    }
}

/// 进程优先级档位（跨平台抽象）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    High,
    AboveNormal,
    Normal,
    BelowNormal,
    Idle,
}

impl Priority {
    pub fn label(&self) -> &'static str {
        match self {
            Priority::High => "高",
            Priority::AboveNormal => "高于正常",
            Priority::Normal => "正常",
            Priority::BelowNormal => "低于正常",
            Priority::Idle => "空闲",
        }
    }

    /// 映射到 Unix nice 值（-20 最高，19 最低）。
    pub fn to_nice(self) -> i32 {
        match self {
            Priority::High => -10,
            Priority::AboveNormal => -5,
            Priority::Normal => 0,
            Priority::BelowNormal => 5,
            Priority::Idle => 19,
        }
    }
}

/// 温度联动：达到阈值后进一步压低 CPU 上限。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TempLink {
    /// 触发温度（摄氏度）。
    pub threshold_c: f32,
    /// 触发后使用的 CPU 上限（百分比，占整机）。
    pub reduced_cpu_percent: f32,
}

/// 单条限速规则。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Rule {
    /// 稳定 ID，用于日志与去重。
    #[serde(default)]
    pub id: String,
    /// 人类可读名称，例如“浏览器省电”。
    #[serde(default)]
    pub name: String,
    /// 进程名匹配表达式：支持 `*` / `?` 通配，否则按不区分大小写子串匹配。
    pub match_name: String,
    /// 排除列表：命中任一表达式则不限制该进程。
    #[serde(default)]
    pub exclude: Vec<String>,
    /// CPU 上限（百分比，占整机总容量，0-100）。
    #[serde(default)]
    pub cpu_limit_percent: Option<f32>,
    /// 内存硬阈值（MiB）。
    #[serde(default)]
    pub memory_limit_mb: Option<u64>,
    /// 内存超阈值后的动作。
    #[serde(default)]
    pub memory_action: MemoryAction,
    /// 绑定优先级（None 表示不调整）。
    #[serde(default)]
    pub priority: Option<Priority>,
    /// 是否启用该规则。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 温度联动（可选）。
    #[serde(default)]
    pub temperature_link: Option<TempLink>,
}

fn default_true() -> bool {
    true
}

impl Rule {
    /// 新建一条仅按名称匹配的空规则。
    pub fn new(match_name: &str) -> Self {
        Rule {
            id: crate::utils::random_token(),
            name: match_name.to_string(),
            match_name: match_name.to_string(),
            exclude: Vec::new(),
            cpu_limit_percent: None,
            memory_limit_mb: None,
            memory_action: MemoryAction::Warn,
            priority: None,
            enabled: true,
            temperature_link: None,
        }
    }

    /// 判断进程名是否命中本规则（含排除列表）。
    pub fn matches_name(&self, process_name: &str) -> bool {
        for ex in &self.exclude {
            if glob_match(ex, process_name) {
                return false;
            }
        }
        glob_match(&self.match_name, process_name)
    }
}

/// 命名规则集。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RuleSet {
    pub name: String,
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// “永不限制”黑名单，命中则任何规则都不作用于该进程。
    #[serde(default)]
    pub blacklist: Vec<String>,
}

impl RuleSet {
    pub fn new(name: &str) -> Self {
        RuleSet {
            name: name.to_string(),
            rules: Vec::new(),
            blacklist: default_blacklist(),
        }
    }

    /// 判断某进程是否被黑名单保护。
    #[allow(dead_code)]
    pub fn is_protected(&self, process_name: &str) -> bool {
        self.blacklist
            .iter()
            .any(|pat| glob_match(pat, process_name))
    }
}

/// 内置默认保护名单：这些是系统关键进程，误限制可能导致桌面/系统崩溃。
pub fn default_blacklist() -> Vec<String> {
    vec![
        // Windows 关键进程
        "System".into(),
        "Registry".into(),
        "smss.exe".into(),
        "csrss.exe".into(),
        "wininit.exe".into(),
        "winlogon.exe".into(),
        "services.exe".into(),
        "lsass.exe".into(),
        "svchost.exe".into(),
        "explorer.exe".into(),
        "dwm.exe".into(),
        "audiodg.exe".into(),
        "MsMpEng.exe".into(),
        "SearchIndexer.exe".into(),
        // Linux / systemd
        "systemd".into(),
        "systemd-*".into(),
        "init".into(),
        "kthreadd".into(),
        "kworker*".into(),
        "ksoftirqd*".into(),
        "migration*".into(),
        "watchdog*".into(),
        "dbus-daemon".into(),
        "udevd".into(),
        "systemd-udevd".into(),
        "rsyslogd".into(),
        "sshd".into(),
        "cron".into(),
        "crond".into(),
        // Android / Termux 关键
        "init*".into(),
        "zygote*".into(),
        "surfaceflinger".into(),
        "logd".into(),
        "vold".into(),
        "healthd".into(),
        "liteprocguard".into(),
    ]
}

/// 进程快照中的一个条目。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    /// 可执行文件路径，取不到时为 "-"。
    pub exe: String,
    /// 启动用户；无法解析时为 UID 数字或 "-"。
    pub user: String,
    /// CPU 占用，百分比（占整机总容量，0-100）。
    pub cpu_percent: f32,
    /// 常驻内存，MiB。
    pub memory_mb: f64,
    /// 是否为系统/服务进程（用于界面标注）。
    pub is_service: bool,
}

/// 简易通配匹配：
///   * 含 `*` / `?` 时按 glob 匹配（`*` 任意多字符，`?` 单字符）；
///   * 否则按不区分大小写的子串匹配（模糊匹配）。
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pat = pattern.trim();
    if pat.is_empty() {
        return false;
    }
    let p_lower = pat.to_ascii_lowercase();
    let t_lower = text.to_ascii_lowercase();

    if pat.contains('*') || pat.contains('?') {
        glob_rec(p_lower.as_bytes(), t_lower.as_bytes())
    } else {
        t_lower.contains(&p_lower)
    }
}

fn glob_rec(pat: &[u8], text: &[u8]) -> bool {
    // 经典回溯匹配，避免递归爆炸。
    let (mut p, mut t) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_t = 0usize;
    while t < text.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = Some(p);
            star_t = t;
            p += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches() {
        assert!(glob_match("chrome*", "chrome.exe"));
        assert!(glob_match("*.exe", "firefox.exe"));
        assert!(glob_match("fire", "Firefox"));
        assert!(!glob_match("chrome", "firefox"));
        assert!(glob_match("a?c", "abc"));
    }

    #[test]
    fn rule_excludes() {
        let mut r = Rule::new("*");
        r.exclude.push("systemd".into());
        assert!(r.matches_name("chrome"));
        assert!(!r.matches_name("systemd"));
    }
}
