//! 通用工具：路径、配置文件读写、日志、随机令牌。
//!
//! 设计原则：
//!   * 不依赖任何外部平台目录库，纯 `std::env` 推导；
//!   * 所有数据都存放在用户本地目录，绝不上传；
//!   * 所有 IO 失败都返回可读中文字符串，而不是 panic。

pub mod config;
pub mod logger;

use std::path::PathBuf;

/// 返回 LiteProcGuard 的本地数据根目录。
///
/// * Unix / Termux：`$XDG_DATA_HOME/liteprocguard`，否则 `$HOME/.local/share/liteprocguard`
/// * Windows：`%APPDATA%\LiteProcGuard`
/// * 其它：当前目录下的 `.liteprocguard`
pub fn base_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LITEPROCGUARD_HOME") {
        return PathBuf::from(dir);
    }

    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("LiteProcGuard");
        }
        return PathBuf::from(".liteprocguard");
    }

    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(xdg).join("liteprocguard");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".local/share/liteprocguard");
        }
        PathBuf::from(".liteprocguard")
    }
}

/// 规则集目录（每个命名规则集一个 `.json` 文件）。
pub fn rules_dir() -> PathBuf {
    base_dir().join("rules")
}

/// 日志目录。
pub fn log_dir() -> PathBuf {
    base_dir().join("logs")
}

/// 运行时目录（存放后台守护的 PID 文件）。
pub fn run_dir() -> PathBuf {
    base_dir().join("run")
}

/// 后台守护 PID 文件路径。
pub fn pid_path() -> PathBuf {
    run_dir().join("liteprocguard.pid")
}

/// 确保所有需要的目录都存在。
pub fn ensure_dirs() -> std::io::Result<()> {
    std::fs::create_dir_all(rules_dir())?;
    std::fs::create_dir_all(log_dir())?;
    std::fs::create_dir_all(run_dir())?;
    Ok(())
}

/// 生成一个短随机令牌，用于 Web UI 的本地访问鉴权。
///
/// 不使用 rand crate，避免额外依赖；用时间戳 + 进程号 + 地址做简单散列即可，
/// 该令牌只用于防止本机浏览器 CSRF，并非密码学安全。
pub fn random_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let mixed = nanos
        .wrapping_mul(0x9E3779B97F4A7C15)
        .wrapping_add(pid.wrapping_mul(0xBF58476D1CE4E5B9));
    format!("{:012x}", (mixed ^ (mixed >> 48)) & 0xffffffffffff)
}

/// 将字节数格式化为人类可读的 MiB 字符串。
#[allow(dead_code)]
pub fn human_mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / 1024.0 / 1024.0)
}
