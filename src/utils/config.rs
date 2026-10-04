//! 配置与规则集的本地读写（纯 JSON）。
//!
//! 目录布局：
//! ```text
//! <base>/
//!   config.json          主配置（默认间隔、温度开关、最近规则集）
//!   rules/<name>.json    每个命名规则集一个文件
//!   logs/                日志
//! ```

use crate::core::model::RuleSet;
use crate::utils;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 主配置。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AppConfig {
    /// 守护默认轮询间隔（秒）。
    #[serde(default = "default_interval")]
    pub default_interval_secs: u64,
    /// 是否启用温度联动。
    #[serde(default)]
    pub temperature_enabled: bool,
    /// 最近使用的规则集名称。
    #[serde(default)]
    pub last_ruleset: String,
}

fn default_interval() -> u64 {
    2
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            default_interval_secs: 2,
            temperature_enabled: false,
            last_ruleset: String::new(),
        }
    }
}

fn config_path() -> PathBuf {
    utils::base_dir().join("config.json")
}

/// 加载主配置；不存在或损坏时返回默认值。
pub fn load_app() -> AppConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// 保存主配置。
pub fn save_app(cfg: &AppConfig) -> Result<(), String> {
    utils::ensure_dirs().map_err(|e| format!("创建数据目录失败：{}", e))?;
    let text = serde_json::to_string_pretty(cfg).map_err(|e| format!("序列化配置失败：{}", e))?;
    std::fs::write(config_path(), text).map_err(|e| format!("写入配置失败：{}", e))
}

/// 将规则集名称转换为安全文件名。
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "default".to_string()
    } else {
        cleaned
    }
}

fn ruleset_path(name: &str) -> PathBuf {
    utils::rules_dir().join(format!("{}.json", sanitize_name(name)))
}

/// 列出所有已保存的规则集名称。
pub fn list_rulesets() -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(utils::rules_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Some(stem) = path.file_stem() {
                    names.push(stem.to_string_lossy().to_string());
                }
            }
        }
    }
    names.sort();
    names
}

/// 加载指定规则集。
pub fn load_ruleset(name: &str) -> Result<RuleSet, String> {
    let path = ruleset_path(name);
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("读取规则集「{}」失败：{}", name, e))?;
    serde_json::from_str(&text).map_err(|e| format!("解析规则集「{}」失败：{}", name, e))
}

/// 保存规则集到 `rules/<name>.json`。
pub fn save_ruleset(rs: &RuleSet) -> Result<PathBuf, String> {
    utils::ensure_dirs().map_err(|e| format!("创建数据目录失败：{}", e))?;
    let path = ruleset_path(&rs.name);
    let text = serde_json::to_string_pretty(rs).map_err(|e| format!("序列化规则集失败：{}", e))?;
    std::fs::write(&path, text).map_err(|e| format!("写入规则集失败：{}", e))?;
    Ok(path)
}

/// 删除指定规则集。
pub fn delete_ruleset(name: &str) -> Result<(), String> {
    let path = ruleset_path(name);
    std::fs::remove_file(&path).map_err(|e| format!("删除规则集「{}」失败：{}", name, e))
}

/// 加载“当前生效”的规则集：优先最近使用，其次第一个已保存，最后内置示例。
pub fn load_active_ruleset() -> RuleSet {
    let app = load_app();
    if !app.last_ruleset.is_empty() {
        if let Ok(rs) = load_ruleset(&app.last_ruleset) {
            return rs;
        }
    }
    if let Some(first) = list_rulesets().into_iter().next() {
        if let Ok(rs) = load_ruleset(&first) {
            return rs;
        }
    }
    default_ruleset()
}

/// 内置默认规则集（首次运行时使用）。
pub fn default_ruleset() -> RuleSet {
    let mut rs = RuleSet::new("默认");
    let mut browser = crate::core::model::Rule::new("chrome");
    browser.name = "浏览器省电".to_string();
    browser.match_name = "*chrome*".to_string();
    browser.cpu_limit_percent = Some(40.0);
    browser.memory_limit_mb = Some(2048);
    browser.memory_action = crate::core::model::MemoryAction::Warn;
    browser.priority = None;
    rs.rules.push(browser);
    rs
}

/// 导出规则集（带元信息的备份文件）。
#[derive(Serialize, Deserialize)]
struct Bundle {
    format: String,
    version: u32,
    ruleset: RuleSet,
}

/// 导出规则集到指定路径。
pub fn export_ruleset(rs: &RuleSet, dest: &str) -> Result<(), String> {
    let bundle = Bundle {
        format: "liteprocguard-ruleset".to_string(),
        version: 1,
        ruleset: rs.clone(),
    };
    let text = serde_json::to_string_pretty(&bundle).map_err(|e| format!("序列化失败：{}", e))?;
    std::fs::write(dest, text).map_err(|e| format!("导出失败：{}", e))
}

/// 从备份文件导入规则集。
pub fn import_ruleset(src: &str) -> Result<RuleSet, String> {
    let text = std::fs::read_to_string(src).map_err(|e| format!("读取失败：{}", e))?;
    // 兼容两种格式：完整 Bundle 或裸 RuleSet。
    if let Ok(bundle) = serde_json::from_str::<Bundle>(&text) {
        Ok(bundle.ruleset)
    } else {
        serde_json::from_str::<RuleSet>(&text).map_err(|e| format!("解析失败：{}", e))
    }
}
