//! BitComet 策略建议器（Task 5-d/T4）。
//!
//! 来源：BitComet 2.21.2 逆向 r1 的两个代码节点：
//! - `adaptive_disk_cache.py`（自适应磁盘缓存，符号 `Core_CachedFile::*` +
//!   `Core_TaskHTTPServer::CachePool`，配置串 `enable_auto_resize_cache` /
//!   `disk_cache_size` / `min_free_memory_to_keep` / `ltseed_cache_size`）
//! - `anti_leech_filter.py`（分级反吸血，符号 `Core_BitTorrent::AntiLeechLevel` +
//!   `BitTorrentTaskWrapper::task_set_anti_leech_level`，配置串
//!   `anti_leech_level` / `enable_client_filter` / `enable_ipfilter`）
//!
//! 分析文档：`docs/research/clients/bitcomet/r1/docs/ANALYSIS.md` §4.3/§4.7；
//! r2 增补：`disk_cache_priority.py`（4 优先级缓存桶）、临时 ban 队列
//! （r2 ANALYSIS.md §21.7「到期自动解除」）。
//!
//! 形态：**纯函数建议器**——输入机器/策略画像，输出可直接写入 libtorrent
//! `settings_pack`（或 btcore 现有钩子）的参数建议。不强行接 libtorrent
//! 运行时；接入点见模块尾部注释。
//!
//! 放置说明：本应放 `crates/btcore/src/strategy.rs`，但 btcore 在 Linux
//! 依赖 bindgen(libclang) + Windows 专用 `lt_kernel.lib`，无法在本环境
//! check/test（Task 5-d 实测：build.rs 因缺 libclang panic）。按任务书的
//! 可编译性决策回退到 `crates/core/src/strategy.rs`（无 FFI 依赖的纯模块），
//! 并在 btcore 侧留一个转发门面（`btcore::strategy`）供 Windows 构建使用。

/// 机器画像（磁盘缓存建议的输入）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheProfile {
    /// 物理内存（MB）。
    pub ram_mb: u64,
    /// 磁盘持续写速度（MB/s）。
    pub disk_speed_mb: u64,
    /// 是否 SSD。
    pub is_ssd: bool,
}

/// 磁盘缓存建议。
///
/// 字段到 libtorrent `settings_pack` 的对应关系：
/// - `cache_size_mb` → `settings_pack::cache_size`（单位 16KiB 块，×64 换算）
/// - `flush_interval_ms` → `settings_pack::cache_expiry`（秒）
/// - `coalesce_writes` → `settings_pack::coalesce_reads/writes`
/// - `auto_resize` / `min_free_memory_mb` → libtorrent 无直接项，属
///   BitComet `enable_auto_resize_cache` 特有，标注为运行时自适应开关。
#[derive(Clone, Debug, PartialEq)]
pub struct DiskCacheAdvice {
    /// 缓存上限（MB）。
    pub cache_size_mb: u64,
    /// 脏块占比上限（0.0-1.0）。
    pub max_dirty_ratio: f64,
    /// 异步 flush 间隔（毫秒）。
    pub flush_interval_ms: u64,
    /// 是否开启内存压力自适应（BitComet `enable_auto_resize_cache`）。
    pub auto_resize: bool,
    /// 自适应时保留的最小可用内存（MB，BitComet `min_free_memory_to_keep`）。
    pub min_free_memory_mb: u64,
    /// 是否合并写（SSD 上减少小 IO）。
    pub coalesce_writes: bool,
    /// 建议依据说明（逐条给出来源符号/阈值）。
    pub notes: Vec<&'static str>,
}

impl From<CacheProfile> for DiskCacheAdvice {
    fn from(p: CacheProfile) -> Self {
        let mut notes: Vec<&'static str> = Vec::new();

        // 基准：内存的 1/8，下限 32MB（r1 auto_resize 收缩地板 32 MiB）、
        // 上限 2048MB。BitComet 桌面端默认档位在 128-512MB。
        let mut size = (p.ram_mb / 8).clamp(32, 2048);
        notes.push("基准 = ram/8，clamp[32, 2048]MB（r1 CachedFileSettings 默认 256MiB + auto_resize 地板 32MiB）");

        // HDD：写放大敏感，封顶 256MB，避免 flush 线程追不上下载速率。
        if !p.is_ssd {
            size = size.min(256);
            notes.push("HDD 封顶 256MB（Core_CachedFile::CachedFileThread 单线程异步 flush 模型）");
        }

        // 慢盘（<60MB/s，低于常见 2.5" HDD 顺序写）：脏数据驻留时间变长，
        // 收缩 25% 换取更低 flush 压力。
        if p.disk_speed_mb < 60 {
            size = size * 3 / 4;
            notes.push("磁盘 <60MB/s：收缩 25%（对应 max_dirty_ratio 收紧，减少突发 IO）");
        }

        // 快 SSD（>=200MB/s）：允许吃到内存 1/4（同 r1 auto_resize 扩容上限
        // 30% 的量级），仍受 2048MB 全局上限约束。
        if p.is_ssd && p.disk_speed_mb >= 200 {
            size = size.max(p.ram_mb / 4).min(2048);
            notes.push("SSD >=200MB/s：扩至 ram/4（r1 auto_resize 扩容上限 30% RAM 量级）");
        }

        let auto_resize = p.ram_mb >= 1024;
        if auto_resize {
            notes.push("ram>=1GB 开启 enable_auto_resize_cache（r1：avail<min_free 缩半、avail>60% 扩 1.5x）");
        }

        // 脏块上限/flush 间隔取 r1 CachedFileSettings 默认值。
        let max_dirty_ratio = if p.is_ssd { 0.6 } else { 0.5 };
        // min_free：r1 默认 256MiB；小内存机器减半防挤占前台。
        let min_free_memory_mb = if p.ram_mb >= 2048 { 256 } else { 128 };

        DiskCacheAdvice {
            cache_size_mb: size,
            max_dirty_ratio,
            flush_interval_ms: 1_000,
            auto_resize,
            min_free_memory_mb,
            coalesce_writes: p.is_ssd,
            notes,
        }
    }
}

impl DiskCacheAdvice {
    /// 换算为 libtorrent `settings_pack::cache_size` 单位（16KiB 块数）。
    pub fn lt_cache_size_blocks(&self) -> i32 {
        (self.cache_size_mb.saturating_mul(64)) as i32
    }
}

/// 反吸血策略画像（建议器的输入）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LeechProfile {
    /// 计划的上传槽位数（对应 BitComet 连接设置里的上传任务数）。
    pub upload_slots: u32,
    /// 目标分享率（seeding 停止条件，仅用于校准 leech 判定阈值）。
    pub ratio_target: f64,
    /// 动态 ban 的窗口期（秒；r2 §21.7 临时 ban 队列到期自动解除）。
    pub ban_window_secs: u64,
}

/// 反吸血参数建议（等级 2-4：LIMIT/AGGRESSIVE/BAN）。
///
/// 语义对齐 r1 `anti_leech_filter.py`：5 级（OFF/SOFT/LIMIT/AGGRESSIVE/BAN），
/// leech 判定 = 客户端指纹（`-XL####-` 迅雷、`-SD####-` 迅雷 Mini、`-XF####-`
/// Xfplay、`-QQ####-` QQDownload、`-NX####-` Net Transport 等）∨ 健康度
/// 评分 < max_score_threshold（默认 30）。
#[derive(Clone, Debug, PartialEq)]
pub struct AntiLeechAdvice {
    /// libtorrent `settings_pack::choking_algorithm` 建议值。
    pub choking_algorithm: &'static str,
    /// `settings_pack::unchoke_slots_limit`。
    pub unchoke_slots_limit: u32,
    /// `settings_pack::num_optimistic_unchoke_slots`。
    pub optimistic_unchoke_slots: u32,
    /// leech 命中后的上传限速百分比（LIMIT_25 → 25%）。
    pub leech_rate_percent: u32,
    /// leech 判定的最低回报率（r1 `min_share_ratio` 默认 0.3）。
    pub min_share_ratio: f64,
    /// 健康度评分阈值（r1 `max_score_threshold` 默认 30）。
    pub score_threshold: f64,
    /// snub 判定阈值（r1 `snub_threshold` 默认 3）。
    pub snub_threshold: u32,
    /// 临时 ban 窗口（秒；0 = 不自动解除）。
    pub ban_window_secs: u64,
    /// 可直接写入 settings_pack 的 (名, 值) 建议对（接入示例见模块尾部）。
    pub settings_pack_hints: Vec<(&'static str, String)>,
    /// 建议依据说明。
    pub notes: Vec<&'static str>,
}

impl From<LeechProfile> for AntiLeechAdvice {
    fn from(p: LeechProfile) -> Self {
        let mut notes = vec![
            "等级语义对齐 Core_BitTorrent::AntiLeechLevel 0-4（OFF/SOFT/LIMIT/AGGRESSIVE/BAN）",
            "LIMIT_25：leech 上传限速到 1/4 带宽（r1 decide() AGGRESSIVE 以下档位）",
        ];

        // BitComet `enable_auto_upload_rate_control`（自动上传速率控制）
        // → libtorrent 的 rate_based choking。
        let choking_algorithm = "rate_based";
        notes.push("choking_algorithm=rate_based（对齐 enable_auto_upload_rate_control）");

        // 槽位数：libtorrent 默认 8；BitComet 桌面档位常见 4-20。
        let unchoke_slots_limit = p.upload_slots.clamp(4, 32);
        if unchoke_slots_limit != p.upload_slots {
            notes.push("上传槽位 clamp[4,32]（libtorrent 默认 8；避免极端配置饿死吞吐）");
        }
        // 乐观 unchoke：按槽位 1/8，至少 1。
        let optimistic = (unchoke_slots_limit / 8).max(1);

        // min_share_ratio：r1 默认 0.3；若用户目标分享率更低（<0.3），
        // leech 判定随之下调（不可能要求回报超过自己目标）。
        let min_share_ratio = p.ratio_target.clamp(0.05, 0.3);
        if min_share_ratio != 0.3 {
            notes.push("min_share_ratio = min(ratio_target, 0.3)（r1 默认 0.3，随目标分享率校准）");
        }

        let mut hints = vec![
            ("choking_algorithm", choking_algorithm.to_string()),
            ("unchoke_slots_limit", unchoke_slots_limit.to_string()),
            ("num_optimistic_unchoke_slots", optimistic.to_string()),
        ];

        // 临时 ban 窗口：settings_pack 无对应项 → 走 btcore 的
        // lt_ban_peer + 应用层定时解封（r2 §21.7 临时 ban 队列）。
        if p.ban_window_secs > 0 {
            hints.push(("ip_filter_ban_window_secs", p.ban_window_secs.to_string()));
            notes.push("BAN 动作走 lt_ban_peer + 临时 ban 队列到期自动解除（r2 ANALYSIS §21.7）");
        }

        AntiLeechAdvice {
            choking_algorithm,
            unchoke_slots_limit,
            optimistic_unchoke_slots: optimistic,
            leech_rate_percent: 25,
            min_share_ratio,
            score_threshold: 30.0,
            snub_threshold: 3,
            ban_window_secs: p.ban_window_secs,
            settings_pack_hints: hints,
            notes,
        }
    }
}

// ---------------------------------------------------------------------------
// 接入点说明（不在本模块实现，避免 FFI 依赖）
// ---------------------------------------------------------------------------
//
// btcore（libtorrent 薄核）当前 v1 契约没有 settings_pack 全量透传，
// 可立即落地/规划中的接入方式：
//
// 1) 上传限速（LIMIT 档，立即可用）：
//    `btcore::engine::BtCore::set_limits(ih, down, up)` —— 对被识别为 leech
//    的任务把 up 收到全量的 `leech_rate_percent`%。
// 2) BAN 档（立即可用）：
//    `BtCore::ban_peer(ih, addr)`（ffi `lt_ban_peer`）+ 本建议的
//    `ban_window_secs` 由调用方维护临时 ban 队列到期解封。
// 3) settings_pack 参数（需 lt_kernel 增设透传，示例）：
//    ```text
//    let adv = AntiLeechAdvice::from(LeechProfile { upload_slots: 12, ratio_target: 1.0, ban_window_secs: 1800 });
//    let cache = DiskCacheAdvice::from(CacheProfile { ram_mb: 8192, disk_speed_mb: 550, is_ssd: true });
//    // lt_kernel 侧新增 lt_set_settings_pack(s, k, v) 后：
//    for (k, v) in &adv.settings_pack_hints { lt_set_settings_pack(sess, k, v); }
//    sess.set_int(settings_pack::cache_size, cache.lt_cache_size_blocks());
//    ```
//    `cache_expiry` 取 `flush_interval_ms / 1000` 秒。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_cache_baseline_scales_with_ram() {
        let a = DiskCacheAdvice::from(CacheProfile { ram_mb: 8192, disk_speed_mb: 150, is_ssd: false });
        // 8192/8 = 1024 → HDD 封顶 256
        assert_eq!(a.cache_size_mb, 256);
        assert_eq!(a.max_dirty_ratio, 0.5);
        assert_eq!(a.flush_interval_ms, 1000);
        assert!(a.auto_resize, "8GB 内存应开自适应");
        assert_eq!(a.min_free_memory_mb, 256);
        assert!(!a.coalesce_writes);
    }

    #[test]
    fn disk_cache_ssd_fast_gets_quarter_ram() {
        let a = DiskCacheAdvice::from(CacheProfile { ram_mb: 8192, disk_speed_mb: 550, is_ssd: true });
        assert_eq!(a.cache_size_mb, 2048, "ram/4 = 2048，恰触全局上限");
        assert_eq!(a.max_dirty_ratio, 0.6);
        assert!(a.coalesce_writes);
    }

    #[test]
    fn disk_cache_slow_disk_shrinks() {
        let a = DiskCacheAdvice::from(CacheProfile { ram_mb: 4096, disk_speed_mb: 40, is_ssd: false });
        // 4096/8 = 512 → HDD 封顶 256 → 慢盘 ×3/4 = 192
        assert_eq!(a.cache_size_mb, 192);
    }

    #[test]
    fn disk_cache_floor_and_ceiling_hold() {
        // 512MB 内存：基准 64MB
        let small = DiskCacheAdvice::from(CacheProfile { ram_mb: 512, disk_speed_mb: 150, is_ssd: true });
        assert_eq!(small.cache_size_mb, 64);
        assert!(!small.auto_resize, "小内存不开自适应");
        assert_eq!(small.min_free_memory_mb, 128);
        // 32GB 内存：基准 4096 → 封顶 2048
        let big = DiskCacheAdvice::from(CacheProfile { ram_mb: 32_768, disk_speed_mb: 550, is_ssd: true });
        assert_eq!(big.cache_size_mb, 2048);
    }

    #[test]
    fn lt_cache_size_unit_conversion() {
        let a = DiskCacheAdvice::from(CacheProfile { ram_mb: 1024, disk_speed_mb: 150, is_ssd: false });
        // 1024/8=128 → HDD 128MB → 128*64 = 8192 块（16KiB/块）
        assert_eq!(a.cache_size_mb, 128);
        assert_eq!(a.lt_cache_size_blocks(), 8192);
    }

    #[test]
    fn anti_leech_defaults_align_r1() {
        let a = AntiLeechAdvice::from(LeechProfile { upload_slots: 8, ratio_target: 1.0, ban_window_secs: 1800 });
        assert_eq!(a.choking_algorithm, "rate_based");
        assert_eq!(a.unchoke_slots_limit, 8);
        assert_eq!(a.optimistic_unchoke_slots, 1);
        assert_eq!(a.leech_rate_percent, 25);
        assert_eq!(a.min_share_ratio, 0.3, "ratio_target=1.0 > 0.3 → 取默认 0.3");
        assert_eq!(a.score_threshold, 30.0);
        assert_eq!(a.snub_threshold, 3);
        assert_eq!(a.ban_window_secs, 1800);
        let hint = |k: &str| {
            a.settings_pack_hints
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(hint("choking_algorithm"), "rate_based");
        assert_eq!(hint("unchoke_slots_limit"), "8");
        assert_eq!(hint("num_optimistic_unchoke_slots"), "1");
        assert_eq!(hint("ip_filter_ban_window_secs"), "1800");
    }

    #[test]
    fn anti_leech_slots_clamped_and_ratio_calibrated() {
        // 极端槽位 clamp
        let a = AntiLeechAdvice::from(LeechProfile { upload_slots: 100, ratio_target: 1.0, ban_window_secs: 0 });
        assert_eq!(a.unchoke_slots_limit, 32);
        assert!(a.ban_window_secs == 0 && !a.settings_pack_hints.iter().any(|(k, _)| k.contains("ban_window")));
        // 低目标分享率 → leech 判定阈值随之下调
        let b = AntiLeechAdvice::from(LeechProfile { upload_slots: 8, ratio_target: 0.2, ban_window_secs: 0 });
        assert_eq!(b.min_share_ratio, 0.2);
        // 零目标（不设）→ 下限 0.05
        let c = AntiLeechAdvice::from(LeechProfile { upload_slots: 8, ratio_target: 0.0, ban_window_secs: 0 });
        assert_eq!(c.min_share_ratio, 0.05);
        // 乐观槽随 clamp 后槽位走
        assert_eq!(a.optimistic_unchoke_slots, 4);
    }
}
