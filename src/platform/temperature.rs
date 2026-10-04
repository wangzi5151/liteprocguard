//! 温度传感器读取（温度联动功能的输入）。
//!
//! * Linux / 树莓派：读取 `/sys/class/thermal/thermal_zone*/temp` 与
//!   `/sys/class/hwmon/hwmon*/temp*_input`，取其中可读的最高温。
//! * Windows / 其它：暂不实现（返回 `None`，界面自动隐藏相关选项）。
//!
//! 所有读取失败都静默降级，绝不导致崩溃。

/// 读取当前最高温度（摄氏度）。无法读取时返回 `None`。
pub fn read_celsius() -> Option<f32> {
    platform::read_celsius()
}

/// 是否存在可用的温度传感器。
pub fn available() -> bool {
    read_celsius().is_some()
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod platform {
    use std::path::PathBuf;

    fn read_millidegrees(path: &PathBuf) -> Option<f32> {
        let text = std::fs::read_to_string(path).ok()?;
        let value: f32 = text.trim().parse().ok()?;
        Some(value / 1000.0)
    }

    pub fn read_celsius() -> Option<f32> {
        let mut best: Option<f32> = None;
        let mut consider = |v: f32| {
            if v > -50.0 && v < 200.0 {
                best = Some(match best {
                    Some(b) if b > v => b,
                    _ => v,
                });
            }
        };

        // thermal_zone
        if let Ok(entries) = std::fs::read_dir("/sys/class/thermal") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with("thermal_zone") {
                    if let Some(v) = read_millidegrees(&entry.path().join("temp")) {
                        consider(v);
                    }
                }
            }
        }

        // hwmon
        if let Ok(entries) = std::fs::read_dir("/sys/class/hwmon") {
            for entry in entries.flatten() {
                if let Ok(files) = std::fs::read_dir(entry.path()) {
                    for file in files.flatten() {
                        let fname = file.file_name();
                        let fname = fname.to_string_lossy();
                        if fname.starts_with("temp") && fname.ends_with("_input") {
                            if let Some(v) = read_millidegrees(&file.path()) {
                                consider(v);
                            }
                        }
                    }
                }
            }
        }

        best
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
mod platform {
    pub fn read_celsius() -> Option<f32> {
        None
    }
}
