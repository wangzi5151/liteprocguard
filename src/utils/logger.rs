//! 本地滚动日志。
//!
//! * 只记录：动作、进程名、PID、时间、附加说明；绝不外发。
//! * 单文件超过 [`MAX_LOG_BYTES`] 自动轮转，最多保留 [`KEEP_BACKUPS`] 个历史文件，
//!   避免长期运行把磁盘写满。
//! * 所有写入失败都被静默忽略，日志系统本身不能成为程序崩溃源。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 单个日志文件的最大字节数（默认 1 MiB）。
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// 保留的历史日志份数。
pub const KEEP_BACKUPS: usize = 3;

static LOCK: Mutex<()> = Mutex::new(());

/// 日志文件路径。
pub fn log_path() -> PathBuf {
    super::log_dir().join("liteprocguard.log")
}

/// 把 Unix 秒转换为 `YYYY-MM-DD HH:MM:SS`（UTC）。
fn format_timestamp(secs: i64) -> String {
    // Howard Hinnant 的 civil_from_days 算法，无需 chrono。
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, m, d, hour, minute, second
    )
}

fn now_string() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format_timestamp(secs)
}

/// 若日志超限则轮转：`log -> log.1 -> log.2 -> log.3`。
fn rotate_if_needed(path: &PathBuf) {
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size < MAX_LOG_BYTES {
        return;
    }
    // 删除最旧的备份。
    let oldest = path.with_extension(format!("log.{}", KEEP_BACKUPS));
    let _ = fs::remove_file(&oldest);
    // 依次向后搬迁。
    for i in (1..KEEP_BACKUPS).rev() {
        let from = path.with_extension(format!("log.{}", i));
        let to = path.with_extension(format!("log.{}", i + 1));
        let _ = fs::rename(&from, &to);
    }
    let _ = fs::rename(path, path.with_extension("log.1"));
}

fn append_line(line: &str) {
    let _guard = LOCK.lock();
    let _ = super::ensure_dirs();
    let path = log_path();
    rotate_if_needed(&path);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{}", line);
    }
}

/// 记录一条动作日志。
///
/// * `action` —— 动作名，如 `CPU_LIMIT` / `MEM_WARN` / `PRIORITY` / `GUARD_START`
/// * `name`   —— 进程名，可为空
/// * `pid`    —— 进程号，0 表示与具体进程无关
/// * `detail` —— 人类可读细节
pub fn log_action(action: &str, name: &str, pid: u32, detail: &str) {
    let target = if pid == 0 {
        "-".to_string()
    } else {
        format!("{}({})", name, pid)
    };
    append_line(&format!(
        "{}\t{}\t{}\t{}",
        now_string(),
        action,
        target,
        detail
    ));
}

/// 记录一条普通信息日志。
#[allow(dead_code)]
pub fn log_info(detail: &str) {
    append_line(&format!("{}\tINFO\t-\t{}", now_string(), detail));
}

/// 记录一条警告日志。
pub fn log_warn(detail: &str) {
    append_line(&format!("{}\tWARN\t-\t{}", now_string(), detail));
}

/// 读取全部日志文本（用于 CLI 查看 / Web 展示 / 导出）。
pub fn read_all() -> String {
    fs::read_to_string(log_path()).unwrap_or_default()
}

/// 读取最近 `n` 行日志。
pub fn tail(n: usize) -> Vec<String> {
    let text = read_all();
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    if lines.len() > n {
        lines = lines.split_off(lines.len() - n);
    }
    lines
}

/// 清空所有日志（含历史轮转文件）。
pub fn clear() {
    let path = log_path();
    let _ = fs::remove_file(&path);
    for i in 1..=KEEP_BACKUPS {
        let _ = fs::remove_file(path.with_extension(format!("log.{}", i)));
    }
}

/// 导出日志到指定路径，返回结果说明。
pub fn export(dest: &str) -> Result<(), String> {
    let text = read_all();
    fs::write(dest, text).map_err(|e| format!("导出日志失败：{}", e))?;
    Ok(())
}
