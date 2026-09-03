//! HTTP 续传段账本（P4 统一进度真源）：
//! `<dest>.part.progress`（JSON）记录已完成段区间，是断点续传的**唯一可信凭据**。
//!
//! 背景（G1/G2 修复）：
//! - 动态分段下载对 .part 预分配（`set_len(total)`）→ 中断后文件长度恒等于
//!   total，旧"按文件长度续传"退化为静默产出稀疏空洞文件。账本以显式段区间
//!   取代文件长度作为进度真源。
//! - 旧 ETag 副文件（`.etag`）"失配仍续传"语义在远端内容变化时会产出
//!   旧前缀+新尾部的混合文件。账本内嵌 ETag，失配即作废重下（与
//!   update_sources 换源语义对齐）。
//!
//! 原子性：账本写入 tmp+rename（与 tasks.json/fastresume 同级）。
//! 防篡改：`validate_segments` 校验段与 FIFO 计划严格对齐，非法账本一律重下。

use crate::range::Probe;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 账本格式版本（不兼容变更时递增；load 拒绝未知版本）。
pub const LEDGER_VERSION: u32 = 1;

/// 段账本：已完成段区间列表（闭区间，与 SegmentManager 的 FIFO 段对齐）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    pub version: u32,
    /// 下载总长（字节）。
    pub total: u64,
    /// 段粒度（字节）——恢复 session 必须沿用同一粒度，否则段计划错位。
    pub min_split: u64,
    /// 内容一致性 token（add 探测所得；失配 → 作废重下）。
    #[serde(default)]
    pub etag: Option<String>,
    /// 内容指纹备援（E26）：Last-Modified 原始串（add 探测所得；与 etag
    /// 各自独立参与 decide 核对；服务器无 ETag 时的续传指纹）。
    #[serde(default)]
    pub last_modified: Option<String>,
    /// 已完成段（闭区间 [start, end]，升序不重叠）。
    #[serde(default)]
    pub done: Vec<(u64, u64)>,
}

impl Ledger {
    /// 新建空账本（全新下载起点）。
    pub fn new(
        total: u64,
        min_split: u64,
        etag: Option<String>,
        last_modified: Option<String>,
    ) -> Self {
        Ledger {
            version: LEDGER_VERSION,
            total,
            min_split: if min_split == 0 {
                crate::segment_manager::DEFAULT_MIN_SPLIT
            } else {
                min_split
            },
            etag,
            last_modified,
            done: Vec::new(),
        }
    }

    /// 已完成字节总数。
    pub fn done_bytes(&self) -> u64 {
        self.done.iter().map(|(s, e)| e - s + 1).sum()
    }

    /// 段合法性校验：done 区间必须与 `min_split` 粒度的 FIFO 段计划严格对齐
    /// （start 为 min_split 整数倍；end = min(start+min_split, total) - 1），
    /// 升序不重叠且落在 [0, total) 内。非法（外部篡改/损坏/粒度疯狂）→ false，
    /// 调用方一律作废重下，绝不信任来路不明的"已完成"声明。
    pub fn validate_segments(&self) -> bool {
        if self.version != LEDGER_VERSION || self.min_split < 1024 || self.done.len() > 100_000 {
            return false;
        }
        let mut prev_end: Option<u64> = None;
        for &(s, e) in &self.done {
            if s >= self.total || e < s || e >= self.total {
                return false;
            }
            if s % self.min_split != 0 {
                return false;
            }
            if e != (s + self.min_split).min(self.total) - 1 {
                return false;
            }
            if let Some(pe) = prev_end {
                if s <= pe {
                    return false;
                }
            }
            prev_end = Some(e);
        }
        true
    }
}

/// 账本路径：`<part>.progress`。
pub fn ledger_path(part: &Path) -> PathBuf {
    let mut s = part.as_os_str().to_os_string();
    s.push(".progress");
    PathBuf::from(s)
}

/// 旧版 ETag 副文件路径：`<part>.etag`（仅作清理用途；新代码不再写入）。
pub fn etag_sidecar_path(part: &Path) -> PathBuf {
    let mut s = part.as_os_str().to_os_string();
    s.push(".etag");
    PathBuf::from(s)
}

/// 原子写账本（tmp + rename）。失败仅告警（进度丢失的代价 = 下次重下，
/// 不值得中断下载）。
pub fn save(path: &Path, ledger: &Ledger) {
    let tmp = path.with_extension("progress.tmp");
    let write = || -> std::io::Result<()> {
        let json = serde_json::to_vec(ledger).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    if let Err(e) = write() {
        tracing::warn!("ledger save 失败 {path:?}: {e}（续传进度可能丢失）");
    }
}

/// 读取账本；缺失/损坏/版本未知 → None（调用方按无账本处理 = 重下）。
pub fn load(path: &Path) -> Option<Ledger> {
    let raw = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str::<Ledger>(&raw) {
        Ok(l) if l.version == LEDGER_VERSION => Some(l),
        _ => None,
    }
}

/// 删除账本（不存在则忽略）。
pub fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// 续传决策结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResumeDecision {
    /// 从账本记录的已完成段恢复（沿用账本粒度）。
    Resume {
        done: Vec<(u64, u64)>,
        min_split: u64,
    },
    /// 作废 .part 与账本，全量重下。
    Restart,
}

/// 依据 .part 现状（文件长度 + 账本）与探测结果决定续传/重下。
/// 决策矩阵（P4 + E26 加固）：
/// 1. part 超过文件总长（源变小）→ 重下；
/// 2. 无账本 → **重下**（预分配 .part 长度不可信——G1：空洞文件假完成）；
/// 3. 账本 total 与探测 total 不一致 → 重下；
/// 4. 指纹核对（E26：etag 与 last_modified 各自独立，任一失败即重下）：
///    双方存在且相等 → 放行（确认服务器文件未变）；
///    双方存在但失配 → 重下（内容变化证据——G2）；
///    账本有而探测无 → 重下（指纹消失：无法确认未变，宁枉勿纵）；
///    账本本就无此指纹 → 放行（无从核对，其余防线仍把关）；
/// 5. 服务器不支持 Range（200/416）→ 重下（段下载依赖 206）；
/// 6. 账本段校验失败 → 重下；
/// 7. 其余 → 从 done 恢复。
///
/// 单一指纹字段的续传核对（E26 加固）。
/// - (Some, Some) 相等 → 放行（指纹确认服务器文件未变）
/// - (Some, Some) 不等 → 拒绝（内容已变，混合文件）
/// - (Some, None) → 拒绝（先前有指纹、本次探测消失：无法确认未变，
///   宁枉勿纵 —— 错误续传产出旧前缀+新尾部的静默损坏，代价远高于重下）
/// - (None, _) → 放行（账本本就无此指纹，无从核对；其余指纹字段、
///   总长核对、段对齐校验仍把关）
fn fingerprint_ok(saved: &Option<String>, fresh: &Option<String>) -> bool {
    match (saved, fresh) {
        (Some(s), Some(f)) => s == f,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

pub fn decide(part_len: u64, ledger: Option<&Ledger>, probe: &Probe) -> ResumeDecision {
    if probe.total.is_some_and(|t| part_len > t) {
        return ResumeDecision::Restart;
    }
    let Some(l) = ledger else {
        return ResumeDecision::Restart;
    };
    if probe.total.is_some_and(|t| l.total != t) {
        return ResumeDecision::Restart;
    }
    // E26 双指纹核对：etag 与 last_modified 各自独立走 fingerprint_ok；
    // 任一字段判定失败即作废（_saved 有而 fresh 无 = 指纹消失，同样拒绝）。
    if !fingerprint_ok(&l.etag, &probe.etag) {
        return ResumeDecision::Restart;
    }
    if !fingerprint_ok(&l.last_modified, &probe.last_modified) {
        return ResumeDecision::Restart;
    }
    if !probe.range_supported || !l.validate_segments() {
        return ResumeDecision::Restart;
    }
    ResumeDecision::Resume {
        done: l.done.clone(),
        min_split: l.min_split,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1024 * 1024;

    fn ledger(total: u64, min_split: u64, done: Vec<(u64, u64)>) -> Ledger {
        Ledger {
            version: LEDGER_VERSION,
            total,
            min_split,
            etag: Some("e".into()),
            last_modified: None,
            done,
        }
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.part");
        let l = ledger(10 * MB, MB, vec![(0, MB - 1), (MB, 2 * MB - 1)]);
        save(&ledger_path(&p), &l);
        assert_eq!(load(&ledger_path(&p)).unwrap(), l);
    }

    #[test]
    fn load_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&ledger_path(&dir.path().join("x.part"))).is_none());
    }

    #[test]
    fn load_corrupt_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = ledger_path(&dir.path().join("x.part"));
        std::fs::write(&p, b"{not json").unwrap();
        assert!(load(&p).is_none());
    }

    #[test]
    fn stale_tmp_from_crash_is_ignored() {
        // 崩溃残留 tmp 文件不影响 load（rename 原子性保证）
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.part");
        let l = ledger(4 * MB, MB, vec![(0, MB - 1)]);
        save(&ledger_path(&p), &l);
        std::fs::write(dir.path().join("f.part.progress.tmp"), b"junk").unwrap();
        assert_eq!(load(&ledger_path(&p)).unwrap(), l);
    }

    #[test]
    fn validate_accepts_fifo_aligned_segments() {
        let ms = 4 * MB;
        let l = ledger(
            10 * MB,
            ms,
            vec![(0, ms - 1), (ms, 2 * ms - 1), (2 * ms, 10 * MB - 1)],
        );
        assert!(l.validate_segments());
        assert_eq!(l.done_bytes(), 10 * MB);
    }

    #[test]
    fn validate_rejects_unaligned_or_overlapping() {
        let ms = 4 * MB;
        // 未对齐起点
        assert!(!ledger(10 * MB, ms, vec![(1, ms)]).validate_segments());
        // end 与 FIFO 计划不符
        assert!(!ledger(10 * MB, ms, vec![(0, ms - 2)]).validate_segments());
        // 重叠
        assert!(!ledger(10 * MB, ms, vec![(0, ms - 1), (0, ms - 1)]).validate_segments());
        // 越界
        assert!(!ledger(10 * MB, ms, vec![(10 * MB, 10 * MB)]).validate_segments());
        // 粒度疯狂（< 1KB）
        assert!(!ledger(10 * MB, 512, vec![]).validate_segments());
        // 尾段必须收敛到 total-1
        assert!(!ledger(10 * MB, ms, vec![(8 * MB, 8 * MB + 1024)]).validate_segments());
    }

    #[test]
    fn decide_restart_without_ledger() {
        // G1 核心回归：无账本的 .part（预分配长度不可信）→ 必须重下
        let probe = Probe {
            range_supported: true,
            etag: Some("e".into()),
            total: Some(100),
            last_modified: None,
            filename: None,
        };
        assert_eq!(decide(100, None, &probe), ResumeDecision::Restart);
    }

    #[test]
    fn decide_restart_on_etag_mismatch() {
        // G2 核心回归：ETag 失配 = 内容变化证据 → 重下
        let probe = Probe {
            range_supported: true,
            etag: Some("new".into()),
            total: Some(4096),
            last_modified: None,
            filename: None,
        };
        let l = ledger(4096, 1024, vec![(0, 1023)]);
        assert_eq!(
            decide(4096, Some(&l), &probe),
            ResumeDecision::Restart,
            "etag 失配必须重下"
        );
    }

    #[test]
    fn decide_resume_on_etag_match() {
        let probe = Probe {
            range_supported: true,
            etag: Some("e".into()),
            total: Some(4096),
            last_modified: None,
            filename: None,
        };
        let l = ledger(4096, 1024, vec![(0, 1023)]);
        assert_eq!(
            decide(4096, Some(&l), &probe),
            ResumeDecision::Resume {
                done: vec![(0, 1023)],
                min_split: 1024
            }
        );
    }

    #[test]
    fn decide_resume_when_either_side_lacks_etag() {
        // 账本无 etag（服务器当时未发）+ 探测有 → 无法证明变化 → 续传
        let mut l = ledger(4096, 1024, vec![(0, 1023)]);
        l.etag = None;
        let probe = Probe {
            range_supported: true,
            etag: Some("fresh".into()),
            total: Some(4096),
            last_modified: None,
            filename: None,
        };
        assert!(matches!(
            decide(4096, Some(&l), &probe),
            ResumeDecision::Resume { .. }
        ));
    }

    #[test]
    fn decide_restart_on_total_mismatch_or_oversize_part() {
        let probe = Probe {
            range_supported: true,
            etag: Some("e".into()),
            total: Some(4096),
            last_modified: None,
            filename: None,
        };
        // 账本 total 不同
        let l = ledger(8192, 1024, vec![]);
        assert_eq!(decide(4096, Some(&l), &probe), ResumeDecision::Restart);
        // part 超长（源变小）
        assert_eq!(
            decide(6144, Some(&ledger(4096, 1024, vec![])), &probe),
            ResumeDecision::Restart
        );
    }

    #[test]
    fn decide_restart_when_range_unsupported() {
        let probe = Probe {
            range_supported: false,
            etag: Some("e".into()),
            total: Some(4096),
            last_modified: None,
            filename: None,
        };
        let l = ledger(4096, 1024, vec![(0, 1023)]);
        assert_eq!(decide(4096, Some(&l), &probe), ResumeDecision::Restart);
    }

    #[test]
    fn decide_restart_on_invalid_segments() {
        let probe = Probe {
            range_supported: true,
            etag: Some("e".into()),
            total: Some(4096),
            last_modified: None,
            filename: None,
        };
        // 篡改账本：未对齐段
        let l = ledger(4096, 1024, vec![(7, 1023)]);
        assert_eq!(decide(4096, Some(&l), &probe), ResumeDecision::Restart);
    }
}
