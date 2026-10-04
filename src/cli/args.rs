//! 极简命令行参数解析（不依赖 clap，保持单文件与低占用）。

/// 解析后的参数集合。
pub struct ArgParser<'a> {
    raw: &'a [String],
}

impl<'a> ArgParser<'a> {
    pub fn new(raw: &'a [String]) -> Self {
        ArgParser { raw }
    }

    /// 是否出现某个开关，例如 `--json` / `-j`。
    pub fn has(&self, long: &str, short: Option<&str>) -> bool {
        self.raw.iter().any(|a| {
            a == long || short.map(|s| a == s).unwrap_or(false) || a == &format!("{}=", long)
        })
    }

    /// 读取字符串参数：支持 `--name value`、`--name=value`、`-n value`。
    pub fn value(&self, long: &str, short: Option<&str>) -> Option<String> {
        for (i, a) in self.raw.iter().enumerate() {
            if let Some(rest) = a.strip_prefix(&format!("{}=", long)) {
                return Some(rest.to_string());
            }
            if a == long || short.map(|s| a == s).unwrap_or(false) {
                if let Some(next) = self.raw.get(i + 1) {
                    if !next.starts_with('-') {
                        return Some(next.clone());
                    }
                }
            }
        }
        None
    }

    /// 收集某个选项的所有取值（用于可重复出现的 `--exclude`）。
    pub fn values(&self, long: &str, short: Option<&str>) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.raw.len() {
            let a = &self.raw[i];
            if let Some(rest) = a.strip_prefix(&format!("{}=", long)) {
                out.push(rest.to_string());
            } else if a == long || short.map(|s| a == s).unwrap_or(false) {
                if let Some(next) = self.raw.get(i + 1) {
                    if !next.starts_with('-') {
                        out.push(next.clone());
                    }
                }
            }
            i += 1;
        }
        out
    }

    /// 读取浮点数参数。
    pub fn f32_value(&self, long: &str, short: Option<&str>) -> Option<f32> {
        self.value(long, short).and_then(|v| v.parse().ok())
    }

    /// 读取无符号整数参数。
    pub fn u64_value(&self, long: &str, short: Option<&str>) -> Option<u64> {
        self.value(long, short).and_then(|v| v.parse().ok())
    }

    /// 收集所有位置参数（不以 `-` 开头，且不是某个选项的值）。
    pub fn positionals(&self, value_flags: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.raw.len() {
            let a = &self.raw[i];
            if a.starts_with('-') {
                if !a.contains('=') && value_flags.iter().any(|f| a == f) {
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }
            out.push(a.clone());
            i += 1;
        }
        out
    }
}
