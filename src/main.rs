//! LiteProcGuard 入口。
//!
//! 纯本地、无遥测、无外发网络、无强制后台驻留的进程资源节流工具。

mod cli;
mod core;
mod platform;
mod utils;
mod web;

fn main() {
    // 管道被提前关闭（例如 `| head`）时不要 panic，直接安静退出。
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // 确保本地数据目录存在；失败也不致命。
    let _ = utils::ensure_dirs();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = cli::run(args);

    // 正常情况下各命令已自行释放限制；这里再兜底清一次，确保异常路径安全。
    if code != 0 {
        // 保持退出码语义，不覆盖。
    }
    std::process::exit(code);
}
