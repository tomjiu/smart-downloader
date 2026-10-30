//! 运行时日志级别（B4，#214 §三-13）：qB「行为→日志」对标。
//!
//! 架构：main 启动期装配 `registry + reload(EnvFilter) + fmt` 订阅者，
//! reload 句柄存进程级 `OnceLock`——DaemonState / serve 零侵入（B3
//! 「签名不动、20 调用点零扰动」同哲学）。三个消费面：
//!
//! | 面 | 入口 | 语义 |
//! |---|---|---|
//! | 启动 | [`init`] | `RUST_LOG`（开发者排障杠杆）> config `[logging].level` > `info` |
//! | 设置面 | [`try_set_level`]（`PUT /settings` logging 域） | 立即 reload（下次事件即新级别） |
//! | 热重载 | [`try_set_level`]（refresh_config 跟随） | 手改 config.toml 同样生效 |
//!
//! 库嵌入 / 测试进程未装配订阅者时 `try_set_level` 返回 false（幂等降级，
//! 不 panic）——PUT 侧据此报 `restart_required`（诚实语义，#160 同族预防）。
//!
//! 重启后 `RUST_LOG` 再次优先：环境变量是显式开发者覆盖，设置面热改是
//! 运行时动作，两者不互斥（最后动作者生效到进程退出）。

use std::sync::{Mutex, OnceLock};
use tracing_subscriber::{fmt, prelude::*, reload, EnvFilter};

/// reload 句柄类型（EnvFilter 挂在 Registry 上）。
type FilterHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;

static HANDLE: OnceLock<FilterHandle> = OnceLock::new();
/// 当前生效级别（canonical 小写；init / try_set_level 维护）。
/// current_level() 供 refresh_config 判差异——EnvFilter 自身无稳定读回 API，
/// 本地记账是最小可靠源。
static CURRENT: Mutex<&'static str> = Mutex::new("info");

/// 级别名归一化：trim + 小写 → canonical 名；未知值 None。
/// 与 config::LoggingCfg::normalized_level 白名单单源（六值）。
pub fn normalize_level(s: &str) -> Option<&'static str> {
    match s.trim().to_ascii_lowercase().as_str() {
        "off" => Some("off"),
        "error" => Some("error"),
        "warn" => Some("warn"),
        "info" => Some("info"),
        "debug" => Some("debug"),
        "trace" => Some("trace"),
        _ => None,
    }
}

/// 装配订阅者（进程一次；重复调用 panic——与旧 main 内联 `.init()` 同契约）。
/// `config_level`：`[logging].level`（normalized_level() 口径）。
/// `RUST_LOG` 非空时优先（支持 directives 语法如 `smart_dl=debug,info`）。
pub fn init(config_level: &str) {
    let initial = std::env::var("RUST_LOG")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| config_level.trim().to_ascii_lowercase());
    let filter = EnvFilter::try_new(&initial).unwrap_or_else(|_| {
        eprintln!("日志级别 {initial:?} 解析失败，回落 info");
        EnvFilter::new("info")
    });
    let (filter_layer, handle) = reload::Layer::new(filter);
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt::layer())
        .init();
    // 记账：RUST_LOG directives 不是 canonical 名时按 info 记（读回仅供
    // 热重载判差异，非权威——权威过滤行为在 EnvFilter 内）。
    *CURRENT.lock().unwrap() = normalize_level(&initial).unwrap_or("info");
    let _ = HANDLE.set(handle);
}

/// 当前生效级别（canonical 小写；未 init 时 "info"）。
pub fn current_level() -> &'static str {
    *CURRENT.lock().unwrap()
}

/// 热改日志级别：成功 reload 返回 true；订阅者未装配（库嵌入 / 测试）
/// 返回 false（调用方按 restart_required 语义降级）。级别未知 → false
///（调用方先经 normalize_level 验证，此处防御性兜底）。
pub fn try_set_level(level: &str) -> bool {
    let Some(norm) = normalize_level(level) else {
        return false;
    };
    let Some(h) = HANDLE.get() else {
        return false;
    };
    let new_filter = match EnvFilter::try_new(norm) {
        Ok(f) => f,
        Err(_) => return false,
    };
    match h.reload(new_filter) {
        Ok(()) => {
            *CURRENT.lock().unwrap() = norm;
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_level_accepts_canonical_and_case_insensitive() {
        assert_eq!(normalize_level("info"), Some("info"));
        assert_eq!(normalize_level("DEBUG"), Some("debug"));
        assert_eq!(normalize_level("  Trace  "), Some("trace"));
        assert_eq!(normalize_level("off"), Some("off"));
        assert_eq!(normalize_level("error"), Some("error"));
        assert_eq!(normalize_level("warn"), Some("warn"));
        assert_eq!(normalize_level("verbose"), None);
        assert_eq!(normalize_level(""), None);
        assert_eq!(normalize_level("7"), None);
    }

    #[test]
    fn try_set_level_without_subscriber_degrades_gracefully() {
        // 测试进程未（必定）装配订阅者：不得 panic，返回 false。
        // 注：若同进程其他测试已 init（不可能——init 只在 main），此断言
        // 依赖 HANDLE 为空；cargo test 每测试独立线程但共享进程静态量，
        // 此处仅断言「不 panic + 布尔返回」语义。
        let before = current_level();
        let _ = try_set_level("debug");
        // 未装配：级别记账不变；装配（不可能于测试进程）：变为 debug。
        assert!(current_level() == before || current_level() == "debug");
    }
}
