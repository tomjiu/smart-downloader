//! 第十三轮：审计 batch10 config.rs 的 Default 单源重构
//!
//! 关键变更：
//!   `BtCfg::default().enabled`  从 false → **true**
//!
//! 潜在冲突：
//!   serve.rs:242  若 `cfg.bt.enabled && cfg.xunlei.enabled` → 直接报错
//!
//! 问题：用户只写 `[xunlei] enabled = true`（不写 [bt] 段）时，
//!       bt.enabled 由 Default 提供 → 改造前 false（可用），改造后 true（启动失败）

#![allow(dead_code)]

use std::collections::HashMap;

/// 简化版配置模型（复刻 batch10 语义）
#[derive(Debug, Clone)]
struct BtCfg {
    enabled: bool,
    max_upload_kb_s: u32,
}

#[derive(Debug, Clone)]
struct XunleiCfg {
    enabled: bool,
}

#[derive(Debug, Clone)]
struct Config {
    bt: BtCfg,
    xunlei: XunleiCfg,
}

// ── 改造前：双源 ──
impl Default for BtCfg {
    fn default() -> Self {
        // 改造前（pr114-review）
        BtCfg { enabled: false, max_upload_kb_s: 0 }
    }
}
impl Default for XunleiCfg {
    fn default() -> Self {
        XunleiCfg { enabled: false }
    }
}

fn config_before() -> Config {
    Config {
        bt: BtCfg { enabled: false, max_upload_kb_s: 0 },
        xunlei: XunleiCfg { enabled: false },
    }
}

// ── 改造后：子表单源 ──
fn config_after() -> Config {
    Config {
        bt: BtCfg { enabled: true, max_upload_kb_s: 0 },   // ← batch10 改的
        xunlei: XunleiCfg { enabled: false },
    }
}

/// 模拟 serve.rs:242 的互斥检查
fn serve_check(cfg: &Config) -> Result<(), String> {
    if cfg.xunlei.enabled {
        if cfg.bt.enabled {
            return Err(
                "配置 [bt].enabled 与 [xunlei].enabled 互斥（两引擎共用 BT 槽位）：请仅启用其一"
                    .into(),
            );
        }
    }
    Ok(())
}

/// 模拟 TOML 部分表解析：[xunlei] 只写 enabled，[bt] 段完全不写
fn parse_partial_toml(xunlei_only: bool, bt_enabled_default: bool) -> Config {
    let mut cfg = Config {
        bt: BtCfg { enabled: bt_enabled_default, max_upload_kb_s: 0 },
        xunlei: XunleiCfg { enabled: false },
    };
    if xunlei_only {
        // 用户写了 [xunlei] enabled = true，没写 [bt] 段
        cfg.xunlei.enabled = true;
        // bt 保持 Default —— 关键点
    }
    cfg
}

#[cfg(test)]
mod round13 {
    use super::*;

    #[test]
    fn bt_enabled_default_conflicts_with_xunlei() {
        println!("=== batch10 config.rs：bt.enabled 默认值变更的影响 ===\n");

        println!("[场景 A] 用户只写 [xunlei] enabled = true，不写 [bt] 段\n");

        // 改造前
        let cfg_before = parse_partial_toml(true, false);
        let r_before = serve_check(&cfg_before);
        println!("  改造前 bt.enabled(Default) = false");
        println!("    serve 启动检查 → {:?}", r_before);

        // 改造后
        let cfg_after = parse_partial_toml(true, true);
        let r_after = serve_check(&cfg_after);
        println!("  改造后 bt.enabled(Default) = true");
        println!("    serve 启动检查 → {:?}", r_after);

        assert!(r_before.is_ok(), "改造前应能启动");
        assert!(r_after.is_err(), "改造后应报互斥错误");

        println!("\n  ❌ 行为变更：改造前可用，改造后**启动直接失败**");
        println!("     用户配置：");
        println!("       [xunlei]");
        println!("       enabled = true");
        println!("     （没有 [bt] 段 —— 按 batch10 之前的行为完全合法）");
        println!("     结果：daemon 起不来，报「bt.enabled 与 xunlei.enabled 互斥」");

        println!("\n[场景 B] 用户什么都不写（全默认）\n");
        let cfg_none_before = config_before();
        let cfg_none_after = config_after();
        println!("  改造前 bt.enabled = {}", cfg_none_before.bt.enabled);
        println!("  改造后 bt.enabled = {}", cfg_none_after.bt.enabled);
        println!("  → 改造后**默认启用 BT**（需 libtorrent 动态库 + 监听端口）");
        println!("  → 若编译未带 --features bt，serve.rs:232 会 warn（不阻断）");
        println!("  → 但语义上：**未配置的组件被默认打开**，与最小权限原则相悖");

        println!("\n=== 根因分析 ===\n");
        println!("  batch10 想修的是：");
        println!("    「用户只写 [bt] max_upload_kb_s = 100 时，enabled 退化为 false」");
        println!("    这个缺陷确实存在（我原报告 P1-b 报了）。");
        println!();
        println!("  修法选了「把 BtCfg::default().enabled 改成 true」——");
        println!("  但这把「部分表」和「完全无 [bt] 段」两种情况混为一谈了：");
        println!("    · 部分表（写了 [bt] 其他键）  → 应继承 true ✓");
        println!("    · 完全无 [bt] 段              → 应保持 false ✗（现在是 true）");

        println!("\n=== 更稳的修法 ===\n");
        println!("  用 `#[serde(default)]` 的字段级默认值区分「表存在」与「表缺失」：");
        println!();
        println!("  // 表级不加 #[serde(default)]，字段级加");
        println!("  #[derive(Deserialize)]");
        println!("  pub struct BtCfg {{");
        println!("      #[serde(default = \"default_bt_enabled\")]");
        println!("      pub enabled: bool,");
        println!("      ...");
        println!("  }}");
        println!();
        println!("  然后在 Config::deserialize 里用 Option<BtCfg> + unwrap_or_default()，");
        println!("  或保留表级 default 但让 default_bt_enabled() 读「是否已有其他键」——");
        println!("  实务上最简做法：**保持 enabled 默认 false**，");
        println!("  但把「部分表退化」的修复放在 `Config::load` 的合并层：");
        println!("    先 `let mut c = Config::default()`, 再用 toml 覆盖已有键");

        println!("\n=== 严重性评估 ===\n");
        println!("  影响面：所有「同时使用迅雷引擎」的用户");
        println!("    - 他们必须显式写 `[bt] enabled = false` 才能启动");
        println!("    - 升级到 0.2.3 后 daemon 直接起不来（配置文件未变）");
        println!("  定性：**P1 升级破坏性变更**（非数据损坏，但服务不可用）");
        println!("  建议：要么回退 enabled 默认值，要么在 CHANGELOG 明确标注 BREAKING");
    }
}
