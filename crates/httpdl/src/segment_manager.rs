//! 动态分段管理器（P0，方案A核心）：维护 pending/done 区间，
//! worker 按 FIFO 动态领取段，粒度 `min_split`（默认 16MB），
//! 支持续传偏移（跳过 [0, offset) 视为已下载）。
//!
//! 参考 aria2 SegmentMan 的按需领取机制（不引入 Piece/BitfieldMan 双层）。
//! 失败语义：段全源失败即整体 Err（由 download_dynamic 处理），
//! 本管理器不做段回收；release 接口 P1 预留。

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
}

/// P0 默认最小段粒度（16MB）。
pub const DEFAULT_MIN_SPLIT: u64 = 16 * 1024 * 1024;

/// 动态分段管理器（内部仅持有一个 FIFO 游标 + 已下载字节统计；
/// 领取按序 → 天然无重叠，done 区间合并无需分段维护）。
pub struct SegmentManager {
    total: u64,
    /// 最小段粒度（字节）。
    min_split: u64,
    /// 下一个待领取段起点（FIFO；初始 = 续传起点 offset）。
    next: u64,
    /// 已下载字节数（含续传偏移）。
    done_bytes: u64,
}

impl SegmentManager {
    /// `offset` = 续传起点（0 = 全新下载）。`min_split` <= 0 时用默认 16MB。
    pub fn new(total: u64, offset: u64, min_split: u64) -> Self {
        let offset = offset.min(total);
        SegmentManager {
            total,
            min_split: if min_split == 0 { DEFAULT_MIN_SPLIT } else { min_split },
            next: offset,
            done_bytes: offset,
        }
    }

    /// FIFO 领取下一段；全部领完 → None。
    pub fn take_segment(&mut self) -> Option<Segment> {
        if self.next >= self.total {
            return None;
        }
        let start = self.next;
        let end = (start + self.min_split - 1).min(self.total - 1);
        self.next = end + 1;
        Some(Segment { start, end })
    }

    /// 段下载完成：累加字节数。
    pub fn complete(&mut self, seg: Segment) {
        self.done_bytes += seg.len();
    }

    /// 已下载字节数（含续传偏移）。
    pub fn done_bytes(&self) -> u64 {
        self.done_bytes
    }

    /// 剩余待下载字节数。
    pub fn pending_bytes(&self) -> u64 {
        self.total - self.next
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
        assert_eq!(m.take_segment(), Some(Segment { start: 0, end: 16 * MB - 1 }));
        assert_eq!(m.take_segment(), Some(Segment { start: 16 * MB, end: 32 * MB - 1 }));
        assert_eq!(m.take_segment(), Some(Segment { start: 32 * MB, end: 48 * MB - 1 }));
        assert_eq!(m.take_segment(), Some(Segment { start: 48 * MB, end: 64 * MB - 1 }));
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
        assert_eq!(m.take_segment(), Some(Segment { start: 24 * MB, end: 40 * MB - 1 }));
        assert_eq!(m.take_segment(), Some(Segment { start: 40 * MB, end: 56 * MB - 1 }));
        assert_eq!(m.take_segment(), Some(Segment { start: 56 * MB, end: 64 * MB - 1 }));
        assert_eq!(m.take_segment(), None);
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
