//! CLI 子命令实现（同时被交互式菜单与 Web UI 复用）。

use crate::cli::args::ArgParser;
use crate::core::engine::{self, Engine, EngineConfig};
use crate::core::model::{MemoryAction, Priority, ProcessInfo, Rule, RuleSet};
use crate::platform::{process, temperature};
use crate::utils::{config, logger};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

// ===========================================================================
// 后台守护管理（供菜单 / Web 使用）
// ===========================================================================

struct GuardHandle {
    running: Arc<AtomicBool>,
    join: JoinHandle<()>,
}

static GUARD: Mutex<Option<GuardHandle>> = Mutex::new(None);

/// PID 文件路径。
fn pid_path() -> std::path::PathBuf {
    crate::utils::pid_path()
}

fn write_pid(pid: u32) -> Result<(), String> {
    let _ = crate::utils::ensure_dirs();
    std::fs::write(pid_path(), pid.to_string()).map_err(|e| format!("写入 PID 文件失败：{}", e))
}

fn read_pid() -> Option<u32> {
    std::fs::read_to_string(pid_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn remove_pid() {
    let _ = std::fs::remove_file(pid_path());
}

/// 独立守护进程是否存活。
pub fn daemon_alive() -> bool {
    match read_pid() {
        Some(pid) if process::pid_exists(pid) => true,
        Some(_) => {
            remove_pid();
            false
        }
        None => false,
    }
}

/// 是否正在后台运行守护（本进程线程或独立守护进程）。
pub fn guard_running() -> bool {
    let local = match GUARD.lock() {
        Ok(slot) => slot
            .as_ref()
            .map(|h| !h.join.is_finished())
            .unwrap_or(false),
        Err(_) => false,
    };
    local || daemon_alive()
}

/// 在后台线程启动守护。
pub fn guard_background(ruleset: RuleSet, cfg: EngineConfig) -> Result<(), String> {
    let mut slot = GUARD.lock().map_err(|_| "内部状态锁损坏".to_string())?;
    if slot
        .as_ref()
        .map(|h| !h.join.is_finished())
        .unwrap_or(false)
    {
        return Err("守护已在运行中。".to_string());
    }
    install_signals();
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    let join = std::thread::Builder::new()
        .name("lpg-guard".to_string())
        .spawn(move || {
            let mut engine = Engine::new(ruleset, cfg);
            engine.run(r);
        })
        .map_err(|e| format!("启动守护线程失败：{}", e))?;
    *slot = Some(GuardHandle { running, join });
    Ok(())
}

/// 停止后台守护并等待限制释放。
pub fn guard_stop() -> bool {
    let handle = match GUARD.lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => None,
    };
    if let Some(h) = handle {
        h.running.store(false, Ordering::Relaxed);
        let _ = h.join.join();
        true
    } else {
        false
    }
}

/// 停止所有守护（本进程线程 + 独立守护进程），并清除全部限制。
pub fn stop_all_guards() -> bool {
    let mut stopped = guard_stop();
    if let Some(pid) = read_pid() {
        if process::pid_exists(pid) {
            // Unix 发 SIGTERM（守护会优雅释放限制）；Windows 终止进程（Job 由系统回收）。
            let _ = process::terminate(pid);
            for _ in 0..30 {
                if !process::pid_exists(pid) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            stopped = true;
        }
        remove_pid();
    }
    engine::emergency_clear();
    stopped
}

// ---------------------------------------------------------------------------
// 信号处理：Ctrl+C / SIGTERM 时置位全局 SHUTDOWN，让守护优雅退出并释放限制。
// ---------------------------------------------------------------------------

#[cfg(unix)]
extern "C" fn on_signal(_sig: i32) {
    engine::SHUTDOWN.store(true, Ordering::SeqCst);
}

fn install_signals() {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    #[cfg(unix)]
    unsafe {
        let handler = on_signal as extern "C" fn(i32);
        libc::signal(libc::SIGINT, handler as usize);
        libc::signal(libc::SIGTERM, handler as usize);
    }
}

// ===========================================================================
// list
// ===========================================================================

pub fn cmd_list(args: &[String]) -> i32 {
    let p = ArgParser::new(args);
    let filter = p.value("--filter", Some("-f")).or_else(|| {
        p.positionals(&["--filter", "-f", "--top", "-n"])
            .into_iter()
            .next()
    });
    let json = p.has("--json", Some("-j"));
    let top = p.u64_value("--top", Some("-n")).unwrap_or(0) as usize;

    let mut processes = process::list_processes();
    if let Some(f) = &filter {
        processes.retain(|pr| crate::core::model::glob_match(f, &pr.name));
    }
    if top > 0 && processes.len() > top {
        processes.truncate(top);
    }

    if json {
        match serde_json::to_string_pretty(&processes) {
            Ok(t) => println!("{}", t),
            Err(e) => {
                eprintln!("JSON 序列化失败：{}", e);
                return 1;
            }
        }
        return 0;
    }

    print_process_table(&processes);
    0
}

#[allow(clippy::print_literal)]
fn print_process_table(processes: &[ProcessInfo]) {
    println!(
        "{:>7}  {:<28} {:<12} {:>7} {:>10}  {:<4} {}",
        "PID", "进程名", "用户", "CPU%", "内存MiB", "服务", "路径"
    );
    println!("{}", "-".repeat(100));
    for p in processes {
        let name = truncate(&p.name, 28);
        let user = truncate(&p.user, 12);
        let exe = truncate(&p.exe, 40);
        println!(
            "{:>7}  {:<28} {:<12} {:>7.1} {:>10.1}  {:<4} {}",
            p.pid,
            name,
            user,
            p.cpu_percent,
            p.memory_mb,
            if p.is_service { "是" } else { "" },
            exe
        );
    }
    println!("\n共 {} 个进程。", processes.len());
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let mut out: String = chars[..max.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

// ===========================================================================
// rule
// ===========================================================================

pub fn cmd_rule_new(args: &[String]) -> i32 {
    let p = ArgParser::new(args);
    let positionals = p.positionals(&[
        "--name",
        "--match",
        "-m",
        "--cpu",
        "-c",
        "--mem",
        "--mem-action",
        "--priority",
        "--exclude",
        "--ruleset",
        "-r",
    ]);
    let match_name = p
        .value("--match", Some("-m"))
        .or_else(|| positionals.first().cloned());
    let match_name = match match_name {
        Some(m) if !m.trim().is_empty() => m,
        _ => {
            eprintln!("用法：liteprocguard rule new <进程名匹配> [--name 名称] [--cpu 25] [--mem 2048] \\");
            eprintln!("      [--mem-action warn|lower|gc|terminate] [--priority idle|below|normal|above|high]");
            eprintln!("      [--exclude 模式]... [--ruleset 规则集名]");
            return 2;
        }
    };

    let mut rule = Rule::new(&match_name);
    if let Some(n) = p.value("--name", None) {
        rule.name = n;
    }
    rule.cpu_limit_percent = p.f32_value("--cpu", Some("-c"));
    rule.memory_limit_mb = p.u64_value("--mem", None);
    if let Some(ma) = p.value("--mem-action", None) {
        rule.memory_action = match ma.to_ascii_lowercase().as_str() {
            "warn" | "warning" => MemoryAction::Warn,
            "lower" | "priority" => MemoryAction::LowerPriority,
            "gc" => MemoryAction::GcHint,
            "terminate" | "kill" => MemoryAction::Terminate,
            other => {
                eprintln!("未知内存动作：{}", other);
                return 2;
            }
        };
    }
    if let Some(pr) = p.value("--priority", None) {
        rule.priority = match pr.to_ascii_lowercase().as_str() {
            "high" => Some(Priority::High),
            "above" => Some(Priority::AboveNormal),
            "normal" => Some(Priority::Normal),
            "below" => Some(Priority::BelowNormal),
            "idle" => Some(Priority::Idle),
            other => {
                eprintln!("未知优先级：{}", other);
                return 2;
            }
        };
    }
    rule.exclude = p.values("--exclude", None);

    let rs_name = p.value("--ruleset", Some("-r"));
    let mut ruleset = match resolve_ruleset(rs_name) {
        Ok(rs) => rs,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    ruleset.rules.push(rule.clone());
    if let Err(e) = persist_ruleset(&ruleset) {
        eprintln!("{}", e);
        return 1;
    }
    println!(
        "已添加规则「{}」到规则集「{}」（匹配：{}，CPU：{}，内存：{}）。",
        rule.name,
        ruleset.name,
        rule.match_name,
        rule.cpu_limit_percent
            .map(|c| format!("{:.0}%", c))
            .unwrap_or_else(|| "不限".to_string()),
        rule.memory_limit_mb
            .map(|m| format!("{} MiB", m))
            .unwrap_or_else(|| "不限".to_string()),
    );
    0
}

pub fn cmd_rule_list(_args: &[String]) -> i32 {
    let names = config::list_rulesets();
    if names.is_empty() {
        println!("暂无已保存的规则集。使用 `liteprocguard rule new` 创建。");
        return 0;
    }
    for name in names {
        match config::load_ruleset(&name) {
            Ok(rs) => {
                println!("规则集「{}」（{} 条规则）", rs.name, rs.rules.len());
                for r in &rs.rules {
                    println!(
                        "  - {:<16} 匹配={:<20} CPU={:<7} 内存={:<10} 优先级={}",
                        r.name,
                        r.match_name,
                        r.cpu_limit_percent
                            .map(|c| format!("{:.0}%", c))
                            .unwrap_or_else(|| "不限".to_string()),
                        r.memory_limit_mb
                            .map(|m| format!("{}MiB", m))
                            .unwrap_or_else(|| "不限".to_string()),
                        r.priority
                            .map(|p| p.label().to_string())
                            .unwrap_or_else(|| "-".to_string()),
                    );
                }
            }
            Err(e) => println!("  [读取失败] {}", e),
        }
    }
    0
}

pub fn cmd_rule_delete(args: &[String]) -> i32 {
    let p = ArgParser::new(args);
    let name = p
        .value("--ruleset", Some("-r"))
        .or_else(|| p.positionals(&["--ruleset", "-r"]).into_iter().next());
    match name {
        Some(n) => match config::delete_ruleset(&n) {
            Ok(()) => {
                println!("已删除规则集「{}」。", n);
                0
            }
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        },
        None => {
            eprintln!("用法：liteprocguard rule delete --ruleset <名称>");
            2
        }
    }
}

// ===========================================================================
// guard
// ===========================================================================

pub fn cmd_guard_start(args: &[String]) -> i32 {
    let (ruleset, cfg) = match build_guard(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };
    install_signals();
    print_permission_hint();
    println!(
        "守护已启动：规则集「{}」，间隔 {}s，CPU 后端：{}，温度联动：{}",
        ruleset.name,
        cfg.interval_secs,
        crate::platform::limiter::CpuLimiter::detect_backend(),
        if cfg.temperature_enabled {
            "开"
        } else {
            "关"
        }
    );
    println!("按 Ctrl+C 停止，并自动释放全部限制。");
    let running = Arc::new(AtomicBool::new(true));
    let mut engine = Engine::new(ruleset, cfg);
    engine.run(running);
    println!("守护已停止。");
    0
}

/// `guard bg`：以独立进程方式后台启动守护（脱离控制终端）。
pub fn cmd_guard_bg(args: &[String]) -> i32 {
    // 先校验参数，错误立即反馈。
    if let Err(e) = build_guard(args) {
        eprintln!("{}", e);
        return 1;
    }
    if guard_running() {
        eprintln!("守护已在运行中。");
        return 1;
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("无法定位自身可执行文件：{}", e);
            return 1;
        }
    };

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("__daemon");
    for a in args {
        cmd.arg(a);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    match cmd.spawn() {
        Ok(child) => {
            println!(
                "守护已在后台启动（PID {}）。使用 `liteprocguard guard stop` 停止并释放限制。",
                child.id()
            );
            0
        }
        Err(e) => {
            eprintln!("后台启动失败：{}", e);
            1
        }
    }
}

/// 隐藏的内部命令：真正的后台守护进程主体。
pub fn cmd_daemon(args: &[String]) -> i32 {
    #[cfg(unix)]
    unsafe {
        // 脱离控制终端与会话，确保关闭终端后仍可存活。
        libc::setsid();
    }

    if let Err(e) = write_pid(std::process::id()) {
        logger::log_warn(&e);
        return 1;
    }

    let (ruleset, cfg) = match build_guard(args) {
        Ok(v) => v,
        Err(e) => {
            logger::log_warn(&e);
            remove_pid();
            return 1;
        }
    };

    install_signals();
    logger::log_action(
        "GUARD_DAEMON",
        "",
        0,
        &format!("后台守护启动，PID {}", std::process::id()),
    );
    let mut engine = Engine::new(ruleset, cfg);
    engine.run(Arc::new(AtomicBool::new(true)));
    remove_pid();
    logger::log_action("GUARD_DAEMON", "", 0, "后台守护退出，限制已释放");
    0
}

pub fn cmd_guard_stop(_args: &[String]) -> i32 {
    if stop_all_guards() {
        println!("守护已停止，限制已释放。");
    } else {
        println!("当前没有正在运行的守护，已执行一次限制清理。");
    }
    0
}

fn build_guard(args: &[String]) -> Result<(RuleSet, EngineConfig), String> {
    let p = ArgParser::new(args);
    let app = config::load_app();
    let rs_name = p.value("--ruleset", Some("-r"));
    let ruleset = resolve_ruleset(rs_name)?;
    let interval = p
        .u64_value("--interval", Some("-i"))
        .unwrap_or(app.default_interval_secs)
        .max(1);
    let temperature_enabled = p.has("--temperature", None) || app.temperature_enabled;
    let dry_run = p.has("--dry-run", None);
    Ok((
        ruleset,
        EngineConfig {
            interval_secs: interval,
            temperature_enabled,
            dry_run,
        },
    ))
}

// ===========================================================================
// 紧急清除
// ===========================================================================

pub fn cmd_clear(_args: &[String]) -> i32 {
    let stopped = stop_all_guards();
    if stopped {
        println!("已停止守护并清除全部限制。");
    } else {
        println!("已尝试清除全部 CPU 限制，并回收残留 cgroup。");
    }
    0
}

// ===========================================================================
// 日志
// ===========================================================================

pub fn cmd_log(args: &[String]) -> i32 {
    let p = ArgParser::new(args);
    if p.has("--clear", None) {
        logger::clear();
        println!("日志已清空。");
        return 0;
    }
    if let Some(dest) = p.value("--export", Some("-o")) {
        match logger::export(&dest) {
            Ok(()) => {
                println!("日志已导出到 {}", dest);
                0
            }
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        }
    } else {
        let tail = if p.has("--all", None) {
            usize::MAX
        } else {
            p.u64_value("--tail", Some("-n")).unwrap_or(50) as usize
        };
        let lines = logger::tail(tail);
        if lines.is_empty() {
            println!("暂无日志。");
        } else {
            for line in lines {
                println!("{}", line);
            }
        }
        0
    }
}

// ===========================================================================
// 预设
// ===========================================================================

/// 内置预设（与 `presets/` 目录中的 JSON 保持一致，作为离线兜底）。
pub fn builtin_presets() -> Vec<RuleSet> {
    vec![power_saving_preset(), thermal_preset()]
}

fn power_saving_preset() -> RuleSet {
    let mut rs = RuleSet::new("省电预设");
    let mut browser = Rule::new("*chrome*");
    browser.name = "浏览器省电".into();
    browser.exclude = vec!["liteprocguard".into()];
    browser.cpu_limit_percent = Some(30.0);
    browser.memory_limit_mb = Some(2048);
    browser.memory_action = MemoryAction::Warn;
    browser.priority = Some(Priority::BelowNormal);

    let mut browser2 = Rule::new("*firefox*");
    browser2.name = "火狐省电".into();
    browser2.cpu_limit_percent = Some(30.0);
    browser2.memory_limit_mb = Some(2048);
    browser2.memory_action = MemoryAction::Warn;

    let mut sync = Rule::new("*sync*");
    sync.name = "后台同步限速".into();
    sync.cpu_limit_percent = Some(15.0);
    sync.priority = Some(Priority::Idle);

    rs.rules = vec![browser, browser2, sync];
    rs
}

fn thermal_preset() -> RuleSet {
    let mut rs = RuleSet::new("编译温控保护");
    let mut build = Rule::new("cc1*");
    build.name = "GCC 前端控温".into();
    build.cpu_limit_percent = Some(70.0);
    build.priority = Some(Priority::BelowNormal);
    build.temperature_link = Some(crate::core::model::TempLink {
        threshold_c: 75.0,
        reduced_cpu_percent: 40.0,
    });

    let mut build2 = Rule::new("rustc");
    build2.name = "Rust 编译控温".into();
    build2.cpu_limit_percent = Some(70.0);
    build2.temperature_link = Some(crate::core::model::TempLink {
        threshold_c: 78.0,
        reduced_cpu_percent: 35.0,
    });

    let mut all = Rule::new("*");
    all.name = "全局限温兜底".into();
    all.cpu_limit_percent = None;
    all.temperature_link = Some(crate::core::model::TempLink {
        threshold_c: 85.0,
        reduced_cpu_percent: 50.0,
    });

    rs.rules = vec![build, build2, all];
    rs
}

pub fn cmd_preset_list(_args: &[String]) -> i32 {
    println!("内置预设：");
    for rs in builtin_presets() {
        println!("  - {}（{} 条规则）", rs.name, rs.rules.len());
    }
    let on_disk = crate::utils::base_dir().join("presets");
    if let Ok(entries) = std::fs::read_dir(&on_disk) {
        let files: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        if !files.is_empty() {
            println!("用户预设目录 {}：", on_disk.display());
            for f in files {
                println!("  - {}", f);
            }
        }
    }
    println!("\n使用 `liteprocguard preset apply <名称>` 应用为规则集。");
    0
}

pub fn cmd_preset_apply(args: &[String]) -> i32 {
    let p = ArgParser::new(args);
    let name = match p
        .value("--name", Some("-n"))
        .or_else(|| p.positionals(&["--name", "-n"]).into_iter().next())
    {
        Some(n) => n,
        None => {
            eprintln!("用法：liteprocguard preset apply <预设名称>");
            return 2;
        }
    };

    let mut chosen: Option<RuleSet> = None;
    for rs in builtin_presets() {
        if rs.name == name || config::sanitize_name(&rs.name) == config::sanitize_name(&name) {
            chosen = Some(rs);
            break;
        }
    }
    if chosen.is_none() {
        let path = crate::utils::base_dir()
            .join("presets")
            .join(format!("{}.json", name));
        if let Ok(rs) = config::import_ruleset(path.to_string_lossy().as_ref()) {
            chosen = Some(rs);
        }
    }
    match chosen {
        Some(rs) => {
            if let Err(e) = persist_ruleset(&rs) {
                eprintln!("{}", e);
                return 1;
            }
            println!("预设「{}」已应用并保存为规则集。", rs.name);
            0
        }
        None => {
            eprintln!(
                "未找到预设「{}」。用 `liteprocguard preset list` 查看可用预设。",
                name
            );
            1
        }
    }
}

// ===========================================================================
// 导入 / 导出
// ===========================================================================

pub fn cmd_import(args: &[String]) -> i32 {
    let p = ArgParser::new(args);
    let file = p
        .value("--file", Some("-f"))
        .or_else(|| p.positionals(&["--file", "-f"]).into_iter().next());
    match file {
        Some(f) => match config::import_ruleset(&f) {
            Ok(rs) => {
                if let Err(e) = persist_ruleset(&rs) {
                    eprintln!("{}", e);
                    return 1;
                }
                println!("已导入规则集「{}」（{} 条规则）。", rs.name, rs.rules.len());
                0
            }
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        },
        None => {
            eprintln!("用法：liteprocguard import <文件.json>");
            2
        }
    }
}

pub fn cmd_export(args: &[String]) -> i32 {
    let p = ArgParser::new(args);
    let file = p.value("--file", Some("-o")).or_else(|| {
        p.positionals(&["--file", "-o", "--ruleset", "-r"])
            .into_iter()
            .next()
    });
    let rs_name = p.value("--ruleset", Some("-r"));
    let file = match file {
        Some(f) => f,
        None => {
            eprintln!("用法：liteprocguard export <输出.json> [--ruleset 名称]");
            return 2;
        }
    };
    match resolve_ruleset(rs_name) {
        Ok(rs) => match config::export_ruleset(&rs, &file) {
            Ok(()) => {
                println!("已导出规则集「{}」到 {}", rs.name, file);
                0
            }
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        },
        Err(e) => {
            eprintln!("{}", e);
            1
        }
    }
}

// ===========================================================================
// 温度 / 状态
// ===========================================================================

pub fn cmd_temp(_args: &[String]) -> i32 {
    match temperature::read_celsius() {
        Some(t) => {
            println!("当前温度：{:.1}°C", t);
            0
        }
        None => {
            println!("未检测到可用的温度传感器（该功能在此设备上不可用）。");
            0
        }
    }
}

pub fn cmd_status(_args: &[String]) -> i32 {
    let backend = crate::platform::limiter::CpuLimiter::detect_backend();
    println!("LiteProcGuard v{}", env!("CARGO_PKG_VERSION"));
    println!("数据目录   : {}", crate::utils::base_dir().display());
    println!("CPU 后续端 : {}", backend);
    println!(
        "守护状态   : {}",
        if guard_running() {
            "运行中"
        } else {
            "未运行"
        }
    );
    println!(
        "温度传感器 : {}",
        if temperature::available() {
            "可用"
        } else {
            "不可用"
        }
    );
    println!(
        "提权状态   : {}",
        if process::is_elevated() {
            "已提权"
        } else {
            "普通用户"
        }
    );
    println!("已保存规则集：{}", config::list_rulesets().join(", "));
    0
}

// ===========================================================================
// 公共辅助
// ===========================================================================

/// 解析要使用的规则集：指定名称则加载，否则用当前生效规则集。
pub fn resolve_ruleset(name: Option<String>) -> Result<RuleSet, String> {
    match name {
        Some(n) if !n.trim().is_empty() => config::load_ruleset(&n),
        _ => Ok(config::load_active_ruleset()),
    }
}

/// 保存规则集并把它记为“最近使用”。
pub fn persist_ruleset(rs: &RuleSet) -> Result<(), String> {
    config::save_ruleset(rs)?;
    let mut app = config::load_app();
    app.last_ruleset = rs.name.clone();
    config::save_app(&app)?;
    Ok(())
}

/// 打印权限提示（哪些功能需要 root / 管理员）。
pub fn print_permission_hint() {
    if !process::is_elevated() {
        println!("提示：CPU 硬限制（cgroup / Job Object）通常需要管理员权限。");
        #[cfg(unix)]
        println!("      可尝试：sudo liteprocguard guard start");
        #[cfg(windows)]
        println!("      请右键以管理员身份运行，或使用管理员 PowerShell。");
        println!("      无权限时将自动回退到温和的信号限速方案。");
    }
}
