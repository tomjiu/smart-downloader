//! state.rs 测试区外置（拆分第一步，纯机械移动零语义改动）。
//! 外壳 use super::* 使各测试 mod 的 `use super::*` 名字解析与原结构一致：
//! 子模块 glob 可见父模块（外壳）私有 use 导入 → 最终全部指向 state 模块。
use super::*;
// 技术债 #2 第三步：测试区按 mod 拆分至本目录（一 mod 一文件，纯移动）。
// 路径不变：state_tests::{tests,bt_alert_tests,...}；子 mod 的
// `use super::*` 现指向本外壳，glob 解析链与原单文件结构同构。
// 原 mod 间外壳项（FakeEngine 家族等）保留于下方。

/// FakeEngine 限速调用记录：（engine_tid, down_kb_s, up_kb_s）。
#[cfg(test)]
type LimitCall = (String, Option<u32>, Option<u32>);

/// 假引擎（持久化恢复测试用）：add 记录输入、可对指定 url 返回错误。
#[cfg(test)]
pub struct FakeEngine {
    kind: EngineKind,
    counter: std::sync::atomic::AtomicU64,
    fail_urls: parking_lot::Mutex<std::collections::HashSet<String>>,
    added: parking_lot::Mutex<Vec<String>>,
    xunlei: parking_lot::Mutex<Vec<Vec<u8>>>,
    /// 已注入的 web seed（(engine_tid, url)，F5 webseed 注入测试用）。
    url_seeds: parking_lot::Mutex<Vec<(String, String)>>,
    /// 已下发的任务级限速（LimitCall 记录，限速重放测试用）。
    limits: parking_lot::Mutex<Vec<LimitCall>>,
    /// status() 额外透出的文件级进度（目录 files 同步测试用；默认空 = 保持旧行为）。
    status_files: parking_lot::Mutex<Vec<FileProgress>>,
    /// 已下发的子文件优先级（(engine_tid, pairs)，优先级重放测试用）。
    #[allow(clippy::type_complexity)] // (tid, [(下标, 优先级)]) 记录类型，测试专用
    prio_calls: parking_lot::Mutex<Vec<(String, Vec<(usize, u32)>)>>,
    /// file_priorities() 的可编程行为：None = 默认成功返回空表；
    /// Some(Err(e)) = 返回该错误（metadata 未就绪模拟）；Some(Ok(v)) = 返回 v。
    prio_readback:
        parking_lot::Mutex<Option<Result<Vec<Option<u32>>, smart_dl_core::types::EngineError>>>,
    /// 已下发的 pause 调用（P4 G5 暂停意图重放测试用）。
    paused: parking_lot::Mutex<Vec<String>>,
    /// 已下发的 resume 调用（P4 G5 运行态恢复重放测试用）。
    resumed: parking_lot::Mutex<Vec<String>>,
    /// 已下发的 remove 调用（(engine_tid, delete_data)，E7 数据处置断言用）。
    removed: parking_lot::Mutex<Vec<(String, bool)>>,
    /// 已下发的 set_task_proxy 调用（(engine_tid, proxy)，E8 热改断言用）。
    proxy_sets: parking_lot::Mutex<Vec<(String, Option<String>)>>,
    /// status().name 可编程回显（E9 名字回填测试用；None = 不透出）。
    status_name: parking_lot::Mutex<Option<String>>,
    /// status() 速率回显（(down, up) B/s，E11 速率缓存测试用；默认 (0,0) 旧行为）。
    status_rates: parking_lot::Mutex<(u64, u64)>,
    /// status() 累计统计回显（(down, up) 字节，E33 上传/分享率测试用；默认 (0,0)）。
    status_totals: parking_lot::Mutex<(u64, u64)>,
    /// status().state 可编程回显（E30 重试 e2e；None = 保持默认）。
    status_state: parking_lot::Mutex<Option<EngineState>>,
    /// 已下发的 set_global_limits 调用（(down, up)，E16 总阀门断言用）。
    global_sets: parking_lot::Mutex<Vec<(Option<u32>, Option<u32>)>>,
    /// set_global_limits 的可编程行为：Some(e) = 恒返回该错误（下发失败模拟）。
    global_limits_err: parking_lot::Mutex<Option<smart_dl_core::types::EngineError>>,
}

#[cfg(test)]
impl FakeEngine {
    pub fn new(kind: EngineKind) -> Self {
        FakeEngine {
            kind,
            counter: std::sync::atomic::AtomicU64::new(1),
            fail_urls: parking_lot::Mutex::new(std::collections::HashSet::new()),
            added: parking_lot::Mutex::new(Vec::new()),
            xunlei: parking_lot::Mutex::new(Vec::new()),
            url_seeds: parking_lot::Mutex::new(Vec::new()),
            limits: parking_lot::Mutex::new(Vec::new()),
            status_files: parking_lot::Mutex::new(Vec::new()),
            prio_calls: parking_lot::Mutex::new(Vec::new()),
            prio_readback: parking_lot::Mutex::new(None),
            paused: parking_lot::Mutex::new(Vec::new()),
            resumed: parking_lot::Mutex::new(Vec::new()),
            removed: parking_lot::Mutex::new(Vec::new()),
            proxy_sets: parking_lot::Mutex::new(Vec::new()),
            status_name: parking_lot::Mutex::new(None),
            status_rates: parking_lot::Mutex::new((0, 0)),
            status_totals: parking_lot::Mutex::new((0, 0)),
            status_state: parking_lot::Mutex::new(None),
            global_sets: parking_lot::Mutex::new(Vec::new()),
            global_limits_err: parking_lot::Mutex::new(None),
        }
    }

    pub fn fail_url(&self, url: &str) {
        self.fail_urls.lock().insert(url.to_string());
    }

    /// 移除 add 失败标记（E30 重试 e2e：模拟故障源恢复后重试成功）。
    pub fn unfail_url(&self, url: &str) {
        self.fail_urls.lock().remove(url);
    }

    pub fn added(&self) -> Vec<String> {
        self.added.lock().clone()
    }

    // 跨 feature 门测试共享的 mock 观测器：部分 feature 组合下无调用点。
    // （模块外置前经 pub mod state 的 crate 根可达性豁免 dead_code，现显式化。）
    #[allow(dead_code)]
    pub fn xunlei_resumes(&self) -> Vec<Vec<u8>> {
        self.xunlei.lock().clone()
    }

    /// 读取已记录的 web seed 注入（(engine_tid, url)，F5 webseed 测试断言用）。
    // 跨 feature 门测试共享的 mock 观测器：部分 feature 组合下无调用点。
    // （模块外置前经 pub mod state 的 crate 根可达性豁免 dead_code，现显式化。）
    #[allow(dead_code)]
    pub fn url_seeds(&self) -> Vec<(String, String)> {
        self.url_seeds.lock().clone()
    }

    /// 读取已下发的任务级限速（LimitCall 记录，限速重放测试断言用）。
    pub fn limits(&self) -> Vec<LimitCall> {
        self.limits.lock().clone()
    }

    /// 读取已下发的 pause 调用（P4 G5 重放测试断言用）。
    pub fn paused_calls(&self) -> Vec<String> {
        self.paused.lock().clone()
    }

    /// 读取已下发的 remove 调用（(engine_tid, delete_data)，E7 数据处置断言用）。
    pub fn removed_calls(&self) -> Vec<(String, bool)> {
        self.removed.lock().clone()
    }

    /// 读取已下发的 set_task_proxy 调用（(engine_tid, proxy)，E8 热改断言用）。
    pub fn proxy_set_calls(&self) -> Vec<(String, Option<String>)> {
        self.proxy_sets.lock().clone()
    }

    /// 设置 status().name 回显值（E9 名字回填测试：模拟引擎已定落盘名）。
    pub fn set_status_name(&self, name: &str) {
        *self.status_name.lock() = Some(name.to_string());
    }

    /// 设置 status() 速率回显（(down, up) B/s；E11 速率缓存测试用）。
    pub fn set_status_rates(&self, down: u64, up: u64) {
        *self.status_rates.lock() = (down, up);
    }

    /// 设置 status() 累计统计回显（(down, up) 字节；E33 上传/分享率测试用）。
    pub fn set_status_totals(&self, down: u64, up: u64) {
        *self.status_totals.lock() = (down, up);
    }

    /// 设置 status().state 回显（E30 重试 e2e：模拟引擎报 Error）。
    pub fn set_status_state(&self, s: EngineState) {
        *self.status_state.lock() = Some(s);
    }

    /// 读取已下发的 set_global_limits 调用（E16 总阀门断言用）。
    pub fn global_sets(&self) -> Vec<(Option<u32>, Option<u32>)> {
        self.global_sets.lock().clone()
    }

    /// 编程 set_global_limits 恒返回某错误（E16 下发失败模拟）。
    pub fn fail_global_limits(&self, e: smart_dl_core::types::EngineError) {
        *self.global_limits_err.lock() = Some(e);
    }

    /// 读取已下发的 resume 调用（P4 G5 重放测试断言用）。
    pub fn resumed_calls(&self) -> Vec<String> {
        self.resumed.lock().clone()
    }

    /// 设置 status() 返回的文件级进度（FTP 目录 files 同步测试注入用）。
    // 跨 feature 门测试共享的 mock 观测器：部分 feature 组合下无调用点。
    // （模块外置前经 pub mod state 的 crate 根可达性豁免 dead_code，现显式化。）
    #[allow(dead_code)]
    pub fn set_status_files(&self, files: Vec<FileProgress>) {
        *self.status_files.lock() = files;
    }

    /// 读取已下发的子文件优先级调用记录（优先级重放测试断言用）。
    #[allow(clippy::type_complexity)]
    pub fn prio_calls(&self) -> Vec<(String, Vec<(usize, u32)>)> {
        self.prio_calls.lock().clone()
    }

    /// 编程 file_priorities() 行为（就绪/未就绪模拟用）。
    pub fn set_prio_readback(
        &self,
        v: Option<Result<Vec<Option<u32>>, smart_dl_core::types::EngineError>>,
    ) {
        *self.prio_readback.lock() = v;
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl DownloadEngine for FakeEngine {
    fn id(&self) -> &str {
        "fake"
    }

    fn kind(&self) -> EngineKind {
        self.kind
    }

    fn capabilities(&self) -> Vec<smart_dl_core::types::Capability> {
        vec![]
    }

    async fn add(
        &self,
        task: &DownloadTask,
    ) -> Result<EngineTaskId, smart_dl_core::types::EngineError> {
        let ident = match &task.source {
            DownloadSource::Http { url, .. } => url.clone(),
            DownloadSource::Magnet(m) => m.clone(),
            DownloadSource::TorrentFile(_) => format!("torrent:{}", task.id),
            _ => task.id.clone(),
        };
        if self.fail_urls.lock().contains(&ident) {
            return Err(smart_dl_core::types::EngineError::Other("fake fail".into()));
        }
        self.added.lock().push(ident);
        Ok(format!(
            "fk{}",
            self.counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ))
    }

    async fn pause(&self, id: &EngineTaskId) -> Result<(), smart_dl_core::types::EngineError> {
        self.paused.lock().push(id.clone());
        Ok(())
    }
    async fn resume(&self, id: &EngineTaskId) -> Result<(), smart_dl_core::types::EngineError> {
        self.resumed.lock().push(id.clone());
        Ok(())
    }
    async fn status(
        &self,
        _id: &EngineTaskId,
    ) -> Result<EngineStatus, smart_dl_core::types::EngineError> {
        let (down_rate, up_rate) = *self.status_rates.lock();
        let (total_downloaded, total_uploaded) = *self.status_totals.lock();
        let mut st = EngineStatus {
            down_rate,
            up_rate,
            total_downloaded,
            total_uploaded,
            files: self.status_files.lock().clone(),
            name: self.status_name.lock().clone(),
            ..EngineStatus::default()
        };
        if let Some(s) = *self.status_state.lock() {
            st.state = s;
        }
        Ok(st)
    }
    async fn remove(
        &self,
        id: &EngineTaskId,
        delete_data: bool,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        self.removed.lock().push((id.to_string(), delete_data));
        Ok(())
    }

    async fn set_task_proxy(
        &self,
        id: &EngineTaskId,
        proxy: Option<String>,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        self.proxy_sets.lock().push((id.to_string(), proxy));
        Ok(())
    }
    async fn peers(
        &self,
        _id: &EngineTaskId,
    ) -> Result<Vec<smart_dl_core::types::PeerInfo>, smart_dl_core::types::EngineError> {
        Ok(vec![])
    }
    async fn update_sources(
        &self,
        _id: &EngineTaskId,
        _urls: Vec<String>,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        Ok(())
    }
    async fn add_url_seed(
        &self,
        id: &EngineTaskId,
        url: &str,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        // 记录（engine_tid, url）供测试断言逐条转发
        self.url_seeds.lock().push((id.clone(), url.to_string()));
        Ok(())
    }

    async fn set_limits(
        &self,
        id: &EngineTaskId,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        // 记录（engine_tid, down, up）供限速重放测试断言
        self.limits.lock().push((id.clone(), down_kb_s, up_kb_s));
        Ok(())
    }

    async fn set_global_limits(
        &self,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        // 记录 (down, up) 供 E16 总阀门下发断言（可编程拒绝：global_limits_err）
        if let Some(e) = &*self.global_limits_err.lock() {
            return Err(e.clone());
        }
        self.global_sets.lock().push((down_kb_s, up_kb_s));
        Ok(())
    }

    async fn set_file_priorities(
        &self,
        id: &EngineTaskId,
        priorities: &[(usize, u32)],
    ) -> Result<(), smart_dl_core::types::EngineError> {
        // readback 错误态同步反映到 set（真实 BT 引擎 metadata 未就绪时
        // set/read 同样失败）——pending 重放测试依赖该一致性
        if let Some(Err(e)) = &*self.prio_readback.lock() {
            return Err(e.clone());
        }
        self.prio_calls
            .lock()
            .push((id.clone(), priorities.to_vec()));
        Ok(())
    }

    async fn file_priorities(
        &self,
        _id: &EngineTaskId,
    ) -> Result<Vec<Option<u32>>, smart_dl_core::types::EngineError> {
        match &*self.prio_readback.lock() {
            None => Ok(vec![Some(4)]), // 默认就绪：单文件表
            Some(Ok(v)) => Ok(v.clone()),
            Some(Err(e)) => Err(e.clone()),
        }
    }
    async fn add_peer(
        &self,
        _id: &EngineTaskId,
        _peer: std::net::SocketAddr,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        Ok(())
    }
    async fn ban_peer(
        &self,
        _id: &EngineTaskId,
        _peer: std::net::SocketAddr,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        Ok(())
    }
    async fn read_piece(
        &self,
        _id: &EngineTaskId,
        _idx: u32,
    ) -> Result<Vec<u8>, smart_dl_core::types::EngineError> {
        Ok(vec![])
    }

    async fn add_xunlei_resume(
        &self,
        data: Vec<u8>,
    ) -> Result<EngineTaskId, smart_dl_core::types::EngineError> {
        self.xunlei.lock().push(data);
        Ok(format!(
            "xr{}",
            self.counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ))
    }
}

mod add_opts_tests;
mod auto_retry_tests;
mod b10_tests;
mod batch_select_tests;
mod bt_alert_tests;
mod bt_name_backfill_tests;
mod cleanup_tests;
mod conflict_tests;
mod ct_eq_tests;
mod ftp_tests;
mod global_limits_tests;
mod list_batch_tests;
mod name_backfill_tests;
mod persist_tests;
mod post_download_tests;
mod rate_cache_tests;
mod scheduled_tests;
mod tags_tests;
mod task_proxy_set_tests;
mod task_proxy_tests;
mod task_rename_tests;
mod tests;
mod torrent_tests;
mod webseed_tests;
