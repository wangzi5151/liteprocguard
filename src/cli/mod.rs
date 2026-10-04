//! 命令行入口：参数模式 + 数字交互式菜单。

pub mod args;
pub mod command;
pub mod menu;

use args::ArgParser;

/// 根据参数分派命令，返回进程退出码。
pub fn run(argv: Vec<String>) -> i32 {
    if argv.is_empty() {
        return menu::run();
    }

    let cmd = argv[0].as_str();
    let rest = &argv[1..];
    let p = ArgParser::new(rest);

    // 隐藏的内部命令：后台守护进程主体（由 `guard bg` 派生）。
    if cmd == "__daemon" {
        return command::cmd_daemon(rest);
    }

    match cmd {
        "help" | "--help" | "-h" => {
            print_help();
            0
        }
        "version" | "--version" | "-V" => {
            println!("LiteProcGuard v{}", env!("CARGO_PKG_VERSION"));
            0
        }
        "list" | "ls" | "ps" => command::cmd_list(rest),
        "status" => command::cmd_status(rest),
        "temp" | "temperature" => command::cmd_temp(rest),
        "clear" | "panic" => command::cmd_clear(rest),
        "import" => command::cmd_import(rest),
        "export" => command::cmd_export(rest),
        "log" | "logs" => command::cmd_log(rest),
        "rule" => run_rule(rest),
        "guard" => run_guard(rest),
        "preset" | "presets" => run_preset(rest),
        "web" | "ui" => {
            let port = p.u64_value("--port", Some("-p")).unwrap_or(7317) as u16;
            match crate::web::serve(port) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("{}", e);
                    1
                }
            }
        }
        other => {
            eprintln!("未知命令：{}", other);
            println!();
            print_help();
            2
        }
    }
}

fn run_rule(rest: &[String]) -> i32 {
    let sub = rest.first().map(|s| s.as_str()).unwrap_or("list");
    let tail = if rest.is_empty() { &[][..] } else { &rest[1..] };
    match sub {
        "new" | "add" => command::cmd_rule_new(tail),
        "list" | "ls" => command::cmd_rule_list(tail),
        "delete" | "del" | "rm" => command::cmd_rule_delete(tail),
        other => {
            eprintln!("未知 rule 子命令：{}（可用：new / list / delete）", other);
            2
        }
    }
}

fn run_guard(rest: &[String]) -> i32 {
    let sub = rest.first().map(|s| s.as_str()).unwrap_or("start");
    let tail = if rest.is_empty() { &[][..] } else { &rest[1..] };
    match sub {
        "start" | "run" | "on" => command::cmd_guard_start(tail),
        "bg" | "background" | "daemon" => command::cmd_guard_bg(tail),
        "stop" | "off" => command::cmd_guard_stop(tail),
        other => {
            // 允许 `guard --interval 5` 直接视为 start。
            if other.starts_with('-') {
                command::cmd_guard_start(rest)
            } else {
                eprintln!("未知 guard 子命令：{}（可用：start / bg / stop）", other);
                2
            }
        }
    }
}

fn run_preset(rest: &[String]) -> i32 {
    let sub = rest.first().map(|s| s.as_str()).unwrap_or("list");
    let tail = if rest.is_empty() { &[][..] } else { &rest[1..] };
    match sub {
        "list" | "ls" => command::cmd_preset_list(tail),
        "apply" | "use" => command::cmd_preset_apply(tail),
        other => {
            // 允许 `preset 省电预设` 直接应用。
            if other.starts_with('-') {
                command::cmd_preset_list(rest)
            } else {
                command::cmd_preset_apply(rest)
            }
        }
    }
}

fn print_help() {
    println!(
        r#"LiteProcGuard v{ver} —— 纯本地跨平台进程资源节流守护

用法：
  liteprocguard                     进入数字交互式菜单（推荐新手）
  liteprocguard list [选项]         列出进程
  liteprocguard guard start [选项]  前台启动守护（Ctrl+C 停止并释放）
  liteprocguard guard bg [选项]     后台启动守护
  liteprocguard guard stop          停止守护并释放限制
  liteprocguard clear               紧急清除全部限制
  liteprocguard rule ...            规则集管理
  liteprocguard preset ...          预设管理
  liteprocguard import <文件>       导入规则集
  liteprocguard export <文件>       导出规则集
  liteprocguard log [选项]          查看 / 导出 / 清空日志
  liteprocguard temp                读取温度
  liteprocguard status              显示状态
  liteprocguard web --port 7317     启动本地 Web UI（仅 127.0.0.1）
  liteprocguard help                显示本帮助

list 选项：
  -f, --filter <关键字>   按进程名模糊过滤
  -n, --top <数量>        只显示前 N 个
  -j, --json              以 JSON 输出（供脚本调用）

guard 选项：
  -i, --interval <秒>     轮询间隔，最小 1（默认 2）
  -r, --ruleset <名称>    指定规则集
      --temperature       启用温度联动
      --dry-run           只记录不实际修改

rule 子命令：
  rule new <匹配> [--name 名称] [--cpu 25] [--mem 2048]
           [--mem-action warn|lower|gc|terminate]
           [--priority high|above|normal|below|idle]
           [--exclude 模式]... [--ruleset 名称]
  rule list
  rule delete --ruleset <名称>

log 选项：
      --tail <N>          显示最近 N 行（默认 50）
      --all               显示全部
  -o, --export <文件>     导出日志
      --clear             清空日志

示例：
  liteprocguard list -f chrome -n 20
  liteprocguard guard start -i 2 --temperature
  liteprocguard rule new "rustc" --cpu 60 --mem 4096 --mem-action lower
  sudo liteprocguard guard start        # 需要 root 才能使用 cgroup 硬限制
"#,
        ver = env!("CARGO_PKG_VERSION")
    );
}
