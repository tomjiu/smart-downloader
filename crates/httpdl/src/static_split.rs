//! 静态分块规划（§14 D11/D25）：
//! `N = clamp(file_size/64MB, 2, 8)`；段不相交 → 无文件锁；不支持 Range → 单连接流式。

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

const SEGMENT_SIZE: u64 = 64 * 1024 * 1024;
const MIN_SEGMENTS: u64 = 2;
const MAX_SEGMENTS: u64 = 8;

/// 段数（D11/D25 公式）。
pub fn segment_count(total: u64) -> usize {
    let n = (total / SEGMENT_SIZE).clamp(MIN_SEGMENTS, MAX_SEGMENTS);
    n as usize
}

/// 公式规划：`split_n(total, segment_count(total))`。
pub fn plan_segments(total: u64) -> Vec<Segment> {
    split_n(total, segment_count(total))
}

/// 等分 n 段：前 `total % n` 段各多 1 字节，段连续覆盖 [0, total)。
/// M4b 用户用例（4 段并行 64MB）用显式 n 绕过公式段数。
pub fn split_n(total: u64, n: usize) -> Vec<Segment> {
    if total == 0 || n == 0 {
        return vec![];
    }
    let n = n as u64;
    let base = total / n;
    let rem = total % n;
    (0..n)
        .map(|i| {
            let start = i * base + i.min(rem);
            let len = base + u64::from(i < rem);
            Segment {
                start,
                end: start + len - 1,
            }
        })
        .collect()
}
