//! 数字交互式菜单，面向不想记长命令的新手。
//!
//! 运行 `liteprocguard` 不带参数即进入本菜单。

use crate::cli::command;
use crate::core::engine::EngineConfig;
use crate::core::model::{MemoryAction, Priority, Rule};
use crate::platform::{process, temperature};
use crate::utils::config;
use std::io::{self, Write};

/// 进入交互式菜单。
pub fn run() -> i32 {
    print_banner();
    loop {
        print_menu();
        let line = read_line();
        match line.trim() {
            "1" => list_processes_top(),
            "2" => new_rule_wizard(),
            "3" => toggle_guard(),
            "4" => view_logs(),
            "5" | "q" | "Q" | "exit" | "quit" | "" => break,
            "6" => {
                command::cmd_clear(&[]);
            }
            "7" => {
                command::cmd_temp(&[]);
            }
            "8" => start_web(),
            other => println!("无效选择：{}（请输入 1-8）", other),
        }
        println!();
    }
    // 退出前确保限制被释放。
    command::guard_stop();
    println!("已退出。若曾施加限制，现已全部释放。");
    0
}

fn print_banner() {
    println!("======================================================");
    println!(
        " LiteProcGuard v{} —— 本地进程资源节流守护",
        env!("CARGO_PKG_VERSION")
    );
    println!(" 纯本地 · 无遥测 · 无强制后台驻留 · 单文件运行");
    println!("======================================================");
    if !process::is_elevated() {
        println!("（提示：CPU 硬限制通常需要 root/管理员；无权限时自动回退到信号限速）");
    }
    println!();
}

fn print_menu() {
    let guard = if command::guard_running() {
        "运行中"
    } else {
        "未运行"
    };
    println!("守护状态：{}", guard);
    println!("---------------- 请选择操作 ----------------");
    println!("  1 = 列出进程");
    println!("  2 = 新建限速规则");
    println!("  3 = 启用/停止守护");
    println!("  4 = 查看日志");
    println!("  5 = 退出");
    println!("  6 = 紧急清除全部限制");
    println!("  7 = 查看温度");
    println!("  8 = 启动 Web UI");
    print!("请输入数字并回车：");
    let _ = io::stdout().flush();
}

fn read_line() -> String {
    let mut s = String::new();
    match io::stdin().read_line(&mut s) {
        Ok(0) => {
            // EOF（管道输入结束）时退出。
            "5".to_string()
        }
        Ok(_) => s,
        Err(_) => "5".to_string(),
    }
}

fn prompt(label: &str) -> String {
    print!("{}", label);
    let _ = io::stdout().flush();
    let mut s = String::new();
    let _ = io::stdin().read_line(&mut s);
    s.trim().to_string()
}

#[allow(clippy::print_literal)]
fn list_processes_top() {
    let mut procs = process::list_processes();
    let filter = prompt("过滤关键字（可留空）：");
    if !filter.is_empty() {
        procs.retain(|p| crate::core::model::glob_match(&filter, &p.name));
    }
    if procs.len() > 40 {
        procs.truncate(40);
    }
    println!();
    println!(
        "{:>7}  {:<26} {:<12} {:>7} {:>10}  {}",
        "PID", "进程名", "用户", "CPU%", "内存MiB", "服务"
    );
    println!("{}", "-".repeat(74));
    for p in &procs {
        let name: String = p.name.chars().take(26).collect();
        let user: String = p.user.chars().take(12).collect();
        println!(
            "{:>7}  {:<26} {:<12} {:>7.1} {:>10.1}  {}",
            p.pid,
            name,
            user,
            p.cpu_percent,
            p.memory_mb,
            if p.is_service { "是" } else { "" }
        );
    }
    println!("\n（最多显示 CPU 占用最高的 40 个进程）");
}

fn new_rule_wizard() {
    println!("\n--- 新建限速规则 ---");
    let match_name = prompt("进程名匹配（支持 * 和 ?，例如 chrome*）：");
    if match_name.is_empty() {
        println!("已取消：匹配不能为空。");
        return;
    }
    let mut rule = Rule::new(&match_name);
    let name = prompt("规则名称（可留空）：");
    if !name.is_empty() {
        rule.name = name;
    }

    let cpu = prompt("CPU 上限百分比（1-100，留空=不限制）：");
    if let Ok(v) = cpu.parse::<f32>() {
        rule.cpu_limit_percent = Some(v.clamp(1.0, 100.0));
    }

    let mem = prompt("内存硬阈值 MiB（留空=不限制）：");
    if let Ok(v) = mem.parse::<u64>() {
        if v > 0 {
            rule.memory_limit_mb = Some(v);
        }
    }

    if rule.memory_limit_mb.is_some() {
        println!("内存超限动作：1=仅告警  2=降低优先级  3=GC提示  4=温和终止");
        match prompt("请选择（默认1）：").as_str() {
            "2" => rule.memory_action = MemoryAction::LowerPriority,
            "3" => rule.memory_action = MemoryAction::GcHint,
            "4" => rule.memory_action = MemoryAction::Terminate,
            _ => rule.memory_action = MemoryAction::Warn,
        }
    }

    println!("优先级：1=高  2=高于正常  3=正常  4=低于正常  5=空闲  回车=不调整");
    rule.priority = match prompt("请选择：").as_str() {
        "1" => Some(Priority::High),
        "2" => Some(Priority::AboveNormal),
        "3" => Some(Priority::Normal),
        "4" => Some(Priority::BelowNormal),
        "5" => Some(Priority::Idle),
        _ => None,
    };

    let exclude = prompt("排除列表（逗号分隔，可留空）：");
    if !exclude.is_empty() {
        rule.exclude = exclude
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    let mut ruleset = config::load_active_ruleset();
    ruleset.rules.push(rule.clone());
    match command::persist_ruleset(&ruleset) {
        Ok(()) => println!("已保存规则「{}」到规则集「{}」。", rule.name, ruleset.name),
        Err(e) => println!("保存失败：{}", e),
    }
}

fn toggle_guard() {
    if command::guard_running() {
        if command::guard_stop() {
            println!("守护已停止，限制已释放。");
        }
        return;
    }
    let app = config::load_app();
    let interval = prompt(&format!(
        "轮询间隔秒数（默认 {}，最小 1）：",
        app.default_interval_secs
    ));
    let interval = interval
        .parse::<u64>()
        .unwrap_or(app.default_interval_secs)
        .max(1);

    let temp_enabled = if temperature::available() {
        prompt("启用温度联动？(y/N)：").eq_ignore_ascii_case("y")
    } else {
        false
    };

    let ruleset = config::load_active_ruleset();
    let cfg = EngineConfig {
        interval_secs: interval,
        temperature_enabled: temp_enabled,
        dry_run: false,
    };
    match command::guard_background(ruleset.clone(), cfg) {
        Ok(()) => println!(
            "守护已启动：规则集「{}」，间隔 {}s。按菜单 3 可停止。",
            ruleset.name, interval
        ),
        Err(e) => println!("启动失败：{}", e),
    }
}

fn view_logs() {
    let lines = crate::utils::logger::tail(30);
    println!("\n--- 最近 30 条日志 ---");
    if lines.is_empty() {
        println!("暂无日志。");
    } else {
        for l in lines {
            println!("{}", l);
        }
    }
}

fn start_web() {
    let port = prompt("Web UI 端口（默认 7317）：")
        .parse::<u16>()
        .unwrap_or(7317);
    crate::web::serve_background(port);
    println!(
        "Web UI 已在后台启动，请查看上方的 http://127.0.0.1:{}/?token=... 地址。",
        port
    );
}
