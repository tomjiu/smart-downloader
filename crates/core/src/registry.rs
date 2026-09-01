//! 引擎注册表（能力路由 §4）与并发队列（配额 D24：BT≤3/HTTP·FTP≤8/Provider≤2 FIFO）。

use crate::source_parse::ed2k::parse_ed2k;
use crate::task::TaskId;
use crate::types::{Capability, DownloadEngine, DownloadSource, EngineKind};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// 路由错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoutingError {
    NoEngineForSource,
    /// v1 明确不支持（Ed2k 解析失败等其他情况）。
    Unsupported(String),
    /// 功能关闭（ftp 引擎未注册等）。
    FeatureDisabled(String),
    /// ed2k 已识别但暂不支持下载（Task 5-a T3）。
    ///
    /// 完整 eMule/eDonkey 引擎在远期（BACKLOG C 段）；此处携带解析出的
    /// 元数据（文件名/字节大小/MD4 十六进制），让 daemon 报错可读。
    Ed2kNotSupported {
        name: String,
        size: u64,
        md4: String,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("engine id already registered: {0}")]
    DuplicateId(String),
}

/// 引擎注册表（§4 路由矩阵 + §3 并发配额）。
#[derive(Default)]
pub struct EngineRegistry {
    engines: HashMap<String, Arc<dyn DownloadEngine>>,
    quotas: HashMap<EngineKind, usize>,
}

impl EngineRegistry {
    pub fn new() -> Self {
        let mut quotas = HashMap::new();
        quotas.insert(EngineKind::Bt, 3);
        quotas.insert(EngineKind::Http, 8);
        quotas.insert(EngineKind::Ftp, 8);
        quotas.insert(EngineKind::Provider, 2);
        EngineRegistry {
            engines: HashMap::new(),
            quotas,
        }
    }

    pub fn register(&mut self, engine: Arc<dyn DownloadEngine>) -> Result<(), RegistryError> {
        let id = engine.id().to_string();
        if self.engines.contains_key(&id) {
            return Err(RegistryError::DuplicateId(id));
        }
        self.engines.insert(id, engine);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn DownloadEngine>> {
        self.engines.get(id).cloned()
    }

    pub fn quota(&self, kind: EngineKind) -> usize {
        self.quotas.get(&kind).copied().unwrap_or(0)
    }

    /// 能力路由（§4 路由矩阵）：Magnet/Torrent→bt；Http/Thunder→http；
    /// Ftp→ftp（未注册 → FeatureDisabled）；Ed2k→已识别则 Ed2kNotSupported
    /// （携带 md4/size 元数据）/解析失败则 Unsupported。
    pub fn select(&self, source: &DownloadSource) -> Result<String, RoutingError> {
        match source {
            DownloadSource::Magnet(_) => self
                .first_with(Capability::Magnet)
                .ok_or(RoutingError::NoEngineForSource),
            DownloadSource::TorrentFile(_) => self
                .first_with(Capability::TorrentFile)
                .ok_or(RoutingError::NoEngineForSource),
            DownloadSource::Http { .. } | DownloadSource::Thunder(_) => self
                .first_with(Capability::Http)
                .ok_or(RoutingError::NoEngineForSource),
            DownloadSource::Ftp { .. } => self
                .first_with(Capability::Ftp)
                .ok_or_else(|| RoutingError::FeatureDisabled("ftp".into())),
            DownloadSource::Ed2k(link) => match parse_ed2k(link) {
                Ok(l) => Err(RoutingError::Ed2kNotSupported {
                    name: l.name,
                    size: l.size,
                    md4: l.md4,
                }),
                Err(e) => Err(RoutingError::Unsupported(format!("ed2k 链接解析失败: {e}"))),
            },
            DownloadSource::XunleiShare(_) => self
                .first_with(Capability::OfflineCache)
                .ok_or(RoutingError::NoEngineForSource),
        }
    }

    fn first_with(&self, cap: Capability) -> Option<String> {
        self.engines
            .values()
            .find(|e| e.capabilities().contains(&cap))
            .map(|e| e.id().to_string())
    }
}

/// 提交结果：已启动 / 超配额进等待队列（FIFO）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QueueOutcome {
    Started,
    Queued,
}

/// 并发队列（D24：BT≤3 / HTTP·FTP≤8 / Provider≤2；超出 Queued FIFO）。
pub struct TaskQueue {
    quotas: HashMap<EngineKind, usize>,
    active: HashMap<EngineKind, usize>,
    waiting: VecDeque<TaskId>,
}

impl Default for TaskQueue {
    fn default() -> Self {
        let mut quotas = HashMap::new();
        quotas.insert(EngineKind::Bt, 3);
        quotas.insert(EngineKind::Http, 8);
        quotas.insert(EngineKind::Ftp, 8);
        quotas.insert(EngineKind::Provider, 2);
        TaskQueue {
            quotas,
            active: HashMap::new(),
            waiting: VecDeque::new(),
        }
    }
}

impl TaskQueue {
    pub fn quota(&self, kind: EngineKind) -> usize {
        self.quotas.get(&kind).copied().unwrap_or(0)
    }

    pub fn submit(&mut self, id: TaskId, kind: EngineKind) -> QueueOutcome {
        let quota = self.quota(kind);
        let act = self.active.entry(kind).or_insert(0);
        if *act < quota {
            *act += 1;
            QueueOutcome::Started
        } else {
            self.waiting.push_back(id);
            QueueOutcome::Queued
        }
    }

    /// 任务结束/失败后释放一个配额；若有等待任务按 FIFO 启动，返回其 id。
    pub fn release(&mut self, kind: EngineKind) -> Option<TaskId> {
        if let Some(a) = self.active.get_mut(&kind) {
            if *a > 0 {
                *a -= 1;
            }
        }
        let next = self.waiting.pop_front()?;
        *self.active.entry(kind).or_insert(0) += 1;
        Some(next)
    }

    pub fn waiting_len(&self) -> usize {
        self.waiting.len()
    }

    pub fn active_count(&self, kind: EngineKind) -> usize {
        self.active.get(&kind).copied().unwrap_or(0)
    }
}
