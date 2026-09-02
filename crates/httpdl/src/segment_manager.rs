//! 动态分段管理器（P0，方案A核心）：维护 pending/done 区间，
//! worker 按 FIFO 动态领取段，粒度 `min_split`（默认 16MB），
//! 支持续传（两种凭据：前缀偏移 `new`；段账本 `new_with_done`）。
//!
//! 参考 aria2 SegmentMan 的按需领取机制（不引入 Piece/BitfieldMan 双层）。
//! 失败语义：段全源失败即整体 Err（由 download_dynamic 处理），
//! 本管理器不做段回收；release 接口 P1 预留。
//!
//! P4 演进：pending 从单游标改为段队列——账本恢复场景的已完成段不在
//! 前缀位置（FIFO 中段完成），必须按队列跳过。`complete` 同步记录
//! done 区间（账本持久化数据源）。

use std::collections::VecDeque;

/// 段（闭区间 [start, end]，长度 = end - start + 1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment {
    pub start: u64,
    pub end: u64,
}

impl Segment {
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// P0 默认最小段粒度（16MB）。
pub const DEFAULT_MIN_SPLIT: u64 = 16 * 1024 * 1024;

/// 动态分段管理器：pending 段队列（FIFO 领取 → 天然无重叠）+
/// 已完成段区间（账本持久化数据源）+ 已下载字节统计。
pub struct SegmentManager {
    total: u64,
    /// 待领取段队列（FIFO）。
    pending: VecDeque<Segment>,
    /// 已下载字节数（含续传凭据折算）。
    done_bytes: u64,
    /// 已完成段区间（升序；`new` 偏移路径不记前缀，`new_with_done` 记全量）。
    done_ranges: Vec<(u64, u64)>,
}

impl SegmentManager {
    /// `offset` = 续传起点（0 = 全新下载；跳过 [0, offset) 视为已下载）。
    /// `min_split` <= 0 时用默认 16MB。兼容旧前缀续传语义。
    pub fn new(total: u64, offset: u64, min_split: u64) -> Self {
        let min_split = if min_split == 0 {
            DEFAULT_MIN_SPLIT
        } else {
            min_split
        };
        let mut pending = VecDeque::new();
        let mut next = offset.min(total);
        while next < total {
            let end = (next + min_split - 1).min(total - 1);
            pending.push_back(Segment { start: next, end });
            next = end + 1;
        }
        SegmentManager {
            total,
            pending,
            done_bytes: offset.min(total),
            done_ranges: Vec::new(),
        }
    }

    /// 段账本恢复：按 `min_split` 粒度生成 [0, total) 的 FIFO 段计划，
    /// 剔除 `done` 中已完成的段（须与计划严格对齐，未匹配的区间按待下处理）。
    /// 完成段计入 done_bytes 与 done_ranges（账本回写保持完整视图）。
    pub fn new_with_done(total: u64, min_split: u64, done: &[(u64, u64)]) -> Self {
        let min_split = if min_split == 0 {
            DEFAULT_MIN_SPLIT
        } else {
            min_split
        };
        let mut m = SegmentManager {
            total,
            pending: VecDeque::new(),
            done_bytes: 0,
            done_ranges: Vec::new(),
        };
        let mut k = 0u64;
        while k * min_split < total {
            let s = k * min_split;
            let e = (s + min_split).min(total) - 1;
            k += 1;
            if done.contains(&(s, e)) {
                m.done_ranges.push((s, e));
                m.done_bytes += e - s + 1;
            } else {
                m.pending.push_back(Segment { start: s, end: e });
            }
        }
        m
    }

    /// FIFO 领取下一段；全部领完 → None。
    pub fn take_segment(&mut self) -> Option<Segment> {
        self.pending.pop_front()
    }

    /// 段下载完成：累加字节数并记录区间（账本数据源）。
    pub fn complete(&mut self, seg: Segment) {
        self.done_bytes += seg.len();
        self.done_ranges.push((seg.start, seg.end));
    }

    /// 已下载字节数（含续传凭据折算）。
    pub fn done_bytes(&self) -> u64 {
        self.done_bytes
    }

    /// 剩余待领取字节数。
    pub fn pending_bytes(&self) -> u64 {
        self.pending.iter().map(|s| s.len()).sum()
    }

    /// 已完成段区间（账本回写用）。
    pub fn done_ranges(&self) -> &[(u64, u64)] {
        &self.done_ranges
    }

    pub fn total(&self) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1024 * 1024;

    #[test]
    fn fresh_manager_covers_whole_file() {
        let mut m = SegmentManager::new(64 * MB, 0, 16 * MB);
        assert_eq!(m.done_bytes(), 0);
        assert_eq!(m.pending_bytes(), 64 * MB);
        assert_eq!(
            m.take_segment(),
            Some(Segment {
                start: 0,
                end: 16 * MB - 1
            })
        );
        assert_eq!(
            m.take_segment(),
            Some(Segment {
                start: 16 * MB,
                end: 32 * MB - 1
            })
        );
        assert_eq!(
            m.take_segment(),
            Some(Segment {
                start: 32 * MB,
                end: 48 * MB - 1
            })
        );
        assert_eq!(
            m.take_segment(),
            Some(Segment {
                start: 48 * MB,
                end: 64 * MB - 1
            })
        );
        assert_eq!(m.take_segment(), None);
    }

    #[test]
    fn tail_segment_shorter_than_min_split() {
        // 40MB 文件，16MB 粒度 → 3 段：16/16/8
        let mut m = SegmentManager::new(40 * MB, 0, 16 * MB);
        let mut segs = Vec::new();
        while let Some(s) = m.take_segment() {
            segs.push(s);
        }
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].len(), 16 * MB);
        assert_eq!(segs[1].len(), 16 * MB);
        assert_eq!(segs[2].len(), 8 * MB);
        assert_eq!(segs[2].end, 40 * MB - 1);
    }

    #[test]
    fn resume_offset_skips_prefix_and_counts_done() {
        // 续传：跳过 [0, 24MB)，从 24MB 开始领取
        let mut m = SegmentManager::new(64 * MB, 24 * MB, 16 * MB);
        assert_eq!(m.done_bytes(), 24 * MB, "续传偏移计入已完成");
        assert_eq!(m.pending_bytes(), 40 * MB);
        assert_eq!(
            m.take_segment(),
            Some(Segment {
                start: 24 * MB,
                end: 40 * MB - 1
            })
        );
        assert_eq!(
            m.take_segment(),
            Some(Segment {
                start: 40 * MB,
                end: 56 * MB - 1
            })
        );
        assert_eq!(
            m.take_segment(),
            Some(Segment {
                start: 56 * MB,
                end: 64 * MB - 1
            })
        );
        assert_eq!(m.take_segment(), None);
    }

    #[test]
    fn new_with_done_skips_completed_segments() {
        // 账本恢复：64MB/16MB → 4 段，中间两段已完成 → 只领取首尾
        let ms = 16 * MB;
        let done = vec![(ms, 2 * ms - 1), (2 * ms, 3 * ms - 1)];
        let mut m = SegmentManager::new_with_done(64 * MB, ms, &done);
        assert_eq!(m.done_bytes(), 2 * ms, "恢复段计入已完成");
        assert_eq!(m.pending_bytes(), 2 * ms);
        assert_eq!(
            m.take_segment(),
            Some(Segment {
                start: 0,
                end: ms - 1
            })
        );
        assert_eq!(
            m.take_segment(),
            Some(Segment {
                start: 3 * ms,
                end: 4 * ms - 1
            })
        );
        assert_eq!(m.take_segment(), None);
        // complete 记录区间（账本回写视图完整：恢复段 + 本次完成段）
        m.complete(Segment {
            start: 0,
            end: ms - 1,
        });
        assert_eq!(m.done_ranges().len(), 3);
    }

    #[test]
    fn new_with_done_unmatched_ranges_stay_pending() {
        // 与计划不对齐的 done 条目（未经 validate 的直接调用方）按待下处理
        let ms = 16 * MB;
        let done = vec![(3, 100)];
        let mut m = SegmentManager::new_with_done(2 * ms, ms, &done);
        assert_eq!(m.pending_bytes(), 2 * ms, "未匹配段不折算已完成");
        assert_eq!(
            m.take_segment(),
            Some(Segment {
                start: 0,
                end: ms - 1
            })
        );
    }

    #[test]
    fn complete_accumulates_done_bytes() {
        let mut m = SegmentManager::new(48 * MB, 0, 16 * MB);
        let s1 = m.take_segment().unwrap();
        let s2 = m.take_segment().unwrap();
        m.complete(s1);
        m.complete(s2);
        assert_eq!(m.done_bytes(), 32 * MB);
        assert_eq!(m.pending_bytes(), 16 * MB);
    }

    #[test]
    fn offset_at_or_beyond_total_has_no_pending() {
        let mut m = SegmentManager::new(16 * MB, 16 * MB, 16 * MB);
        assert_eq!(m.pending_bytes(), 0);
        assert_eq!(m.take_segment(), None);
        let mut m2 = SegmentManager::new(16 * MB, 20 * MB, 16 * MB);
        assert_eq!(m2.pending_bytes(), 0);
        assert_eq!(m2.take_segment(), None);
    }

    #[test]
    fn zero_size_file_has_no_segment() {
        let mut m = SegmentManager::new(0, 0, 16 * MB);
        assert_eq!(m.take_segment(), None);
        assert_eq!(m.done_bytes(), 0);
    }

    #[test]
    fn zero_min_split_falls_back_to_default() {
        let mut m = SegmentManager::new(20 * MB, 0, 0);
        // 默认 16MB 粒度 → 2 段
        assert_eq!(m.take_segment().unwrap().len(), 16 * MB);
        assert_eq!(m.take_segment().unwrap().len(), 4 * MB);
    }
}
