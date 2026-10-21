//! BT 域：alert 应用（apply_bt_alert）、活跃任务枚举（active_bt_tids）、.torrent 元数据解析（TorrentMeta 家族）与 bencode 工具族（cfg(feature = "bt")）。

use super::*;

/// 单文件 .torrent 元数据（迅雷导入用）。
#[derive(Debug, Clone)]
pub struct TorrentMeta {
    pub info_hash: String,
    pub piece_length: u32,
    pub pieces_hash: Vec<[u8; 20]>,
    pub name: String,
    /// 单文件大小（仅单文件 torrent 使用）。
    pub file_size: u64,
    /// 多文件列表（仅多文件 torrent 使用）。
    pub files: Vec<FileMeta>,
}

/// 单文件元数据。
#[derive(Debug, Clone)]
pub struct FileMeta {
    /// 相对路径（多文件）或文件名（单文件）。
    pub path: String,
    /// 文件大小（字节）。
    pub size: u64,
    /// 该文件在 torrent 中的起始 piece 索引。
    pub piece_offset: usize,
    /// 该文件占用的 piece 数量。
    pub piece_count: usize,
}

#[cfg(feature = "bt")]
impl TorrentMeta {
    /// 从 .torrent 字节解析元数据（单文件/多文件）。
    ///
    /// TD-1 统一：info dict 定位走 `core::bencode::locate_dict_value`（原始
    /// span，SHA1 哈希语义），字段提取走 `core::bencode::decode` 物化树。
    /// 物化模型下全字段先可见再计算偏移——旧 span 扫描器的 P0-1 时序风险
    /// （字典序下 files 先于 piece length 出现，扫描到 files 时 plen=0）在
    /// 结构上消除。安全校验全部保留：V3 路径净化 / P0-11 checked_add /
    /// MAX_PIECE_LENGTH 上限 / pieces 对齐。
    #[cfg(feature = "bt")]
    pub fn parse(b: &[u8]) -> Result<Self, DaemonError> {
        use sha1::Digest;
        let (info_start, info_end) = smart_dl_core::bencode::locate_dict_value(b, b"info")
            .ok_or_else(|| {
                DaemonError::InvalidSource(".torrent 解析失败：无法定位 info dict".into())
            })?;
        if b.get(info_start) != Some(&b'd') {
            return Err(DaemonError::InvalidSource(
                ".torrent info 字段不是字典".into(),
            ));
        }

        let info_hash = {
            let digest = sha1::Sha1::digest(&b[info_start..=info_end]);
            digest
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect::<String>()
        };

        let info: smart_dl_core::bencode::Value =
            smart_dl_core::bencode::decode(&b[info_start..=info_end]).map_err(|e| {
                DaemonError::InvalidSource(format!(".torrent info dict 解析失败: {e}"))
            })?;

        // piece length：非零 + 上限（与 libtorrent 一致：1 << 24 = 16 MiB）。
        // 恶意种子的超大 piece_length 会在下游偏移计算中溢出回绕
        // （xltd 块扫描、piece 偏移映射），release 下静默读错位置。
        const MAX_PIECE_LENGTH: u32 = 1 << 24;
        let piece_length = info
            .dict_get(b"piece length")
            .and_then(|v| v.as_int())
            .ok_or_else(|| {
                DaemonError::InvalidSource(".torrent 缺少必要字段 (piece length/pieces)".into())
            })? as u32;
        if piece_length == 0 || piece_length > MAX_PIECE_LENGTH {
            return Err(DaemonError::InvalidSource(format!(
                "piece length 超过上限 {MAX_PIECE_LENGTH}（得到 {piece_length}）"
            )));
        }

        // pieces：20 字节/条对齐，余数不得静默丢弃。
        let pieces_data = info
            .dict_get(b"pieces")
            .and_then(|v| v.as_bytes())
            .ok_or_else(|| {
                DaemonError::InvalidSource(".torrent 缺少必要字段 (piece length/pieces)".into())
            })?;
        if pieces_data.is_empty() || pieces_data.len() % 20 != 0 {
            return Err(DaemonError::InvalidSource(
                "pieces 长度不是 20 的倍数".into(),
            ));
        }
        let pieces_hash = pieces_data.as_chunks::<20>().0.to_vec();

        // name：utf-8 lossy + V3 净化（torrent 根名直通 dest_root.join，
        // 恶意 name（../、绝对路径）即任意文件写——parse 层即拒任务）。
        let name = info
            .dict_get(b"name")
            .and_then(|v| v.as_bytes())
            .map(|raw| String::from_utf8_lossy(raw).into_owned())
            .ok_or_else(|| DaemonError::InvalidSource("name 解析失败".into()))?;
        smart_dl_core::session::output::sanitize_rel(&name).map_err(|_| {
            DaemonError::InvalidSource(format!(".torrent name 含非法路径分量已拒绝: {name}"))
        })?;

        if let Some(files_v) = info.dict_get(b"files") {
            // 多文件 torrent：逐文件提取 (path, length)，V3 逐段净化 +
            // P0-11 累计溢出拒绝 + piece 偏移计算（plen 已校验非零）。
            let entries = files_v
                .as_list()
                .ok_or_else(|| DaemonError::InvalidSource("files 解析失败".into()))?;
            let plen = piece_length as u64;
            let mut files = Vec::with_capacity(entries.len());
            let mut total_size: u64 = 0;
            for entry in entries {
                let length = entry
                    .dict_get(b"length")
                    .and_then(|v| v.as_int())
                    .ok_or_else(|| DaemonError::InvalidSource("files length 解析失败".into()))?
                    as u64;
                let path_parts = entry
                    .dict_get(b"path")
                    .and_then(|v| v.as_list())
                    .ok_or_else(|| DaemonError::InvalidSource("files path list 解析失败".into()))?;
                let mut parts = Vec::with_capacity(path_parts.len());
                for seg_v in path_parts {
                    let seg = seg_v.as_bytes().ok_or_else(|| {
                        DaemonError::InvalidSource("path segment 解析失败".into())
                    })?;
                    let seg_str = String::from_utf8_lossy(seg).into_owned();
                    if seg_str == ".."
                        || seg_str.contains('/')
                        || seg_str.contains('\\')
                        || seg_str.is_empty()
                    {
                        return Err(DaemonError::InvalidSource(format!(
                            "files path 含非法段已拒绝: {seg_str}"
                        )));
                    }
                    parts.push(seg_str);
                }
                if parts.is_empty() {
                    return Err(DaemonError::InvalidSource("files path 为空".into()));
                }
                let path = parts.join(std::path::MAIN_SEPARATOR_STR);

                if length > 0 && !path.is_empty() {
                    let piece_offset = (total_size / plen) as usize;
                    let piece_count = length.div_ceil(plen) as usize;
                    total_size = total_size.checked_add(length).ok_or_else(|| {
                        DaemonError::InvalidSource("files 累计大小溢出 u64".into())
                    })?;
                    files.push(FileMeta {
                        path,
                        size: length,
                        piece_offset,
                        piece_count,
                    });
                }
            }
            Ok(Self {
                info_hash,
                piece_length,
                pieces_hash,
                name,
                file_size: 0,
                files,
            })
        } else {
            // 单文件 torrent
            let file_size = info
                .dict_get(b"length")
                .and_then(|v| v.as_int())
                .ok_or_else(|| DaemonError::InvalidSource(".torrent 缺少 length 字段".into()))?
                as u64;
            Ok(Self {
                info_hash,
                piece_length,
                pieces_hash,
                name,
                file_size,
                files: vec![],
            })
        }
    }
}

impl DaemonState {
    /// 活跃 BT 任务的 engine_tid 列表（fastresume 周期/退出保存范围，P4 G4）。
    /// 非终态即保存（Queued/Evaluating/Downloading/Paused/FallbackProvider/
    /// Transferring/Seeding——做种中也保存，防"部分校验进度丢失"）；
    /// Completed/Failed/Stopped 跳过。
    #[cfg(feature = "bt")]
    pub fn active_bt_tids(&self) -> Vec<String> {
        self.tasks
            .lock()
            .values()
            .filter(|r| r.engine_kind == EngineKind::Bt)
            .filter(|r| {
                !matches!(
                    r.task.state,
                    TaskState::Completed | TaskState::Failed | TaskState::Stopped
                )
            })
            .filter_map(|r| r.engine_tid.clone())
            .collect()
    }

    /// 任务当前 engine_tid（引擎侧 id；BT=infohash）。未注册/任务不存在 → None。
    pub fn engine_tid_of(&self, id: &str) -> Option<String> {
        self.tasks.lock().get(id).and_then(|r| r.engine_tid.clone())
    }

    /// 应用一条 BT alert 到匹配任务（engine_tid 大小写不敏感归一化比较）：
    /// 状态迁移（`bt_events::transition_for`）+ 引擎缓存写入；返回效果供广播。
    /// E30 对齐（A2）：Error alert 命中活跃任务（Queued/Downloading）时经
    /// `fail_or_schedule_retry` 拦截——预算未用尽回 Queued 安排退避重激活，
    /// 广播目标为拦截后的实际状态；Paused/Seeding 下保持旧直终语义。
    /// 无匹配任务或无迁移 → `None`（调用方丢弃该 alert）。
    ///
    /// Bug B 根因修复：`autosave()` 必须在 tasks 锁【外】调用——autosave →
    /// persisted_tasks 会再次取同一把非重入 std Mutex，锁内调用 = 同线程重入
    /// 自死锁（alert 循环线程永久持锁挂死 → 全部端点含 /config 无限 hang）。
    /// 修复前证据：`aba HIT → autosave BEGIN` 后日志静默 + tasks_free=false 永续。
    #[cfg(feature = "bt")]
    pub fn apply_bt_alert(&self, a: &smart_dl_btcore::Alert) -> Option<BtAlertEffect> {
        let ih_l = a.ih.to_ascii_lowercase();
        let effect = {
            let mut tasks = self.tasks.lock();
            let mut found: Option<BtAlertEffect> = None;
            for (id, rec) in tasks.iter_mut() {
                if rec.engine_kind != EngineKind::Bt {
                    continue;
                }
                let Some(tid) = &rec.engine_tid else {
                    continue;
                };
                if tid.to_ascii_lowercase() != ih_l {
                    continue;
                }
                // 命中任务（每条 alert 至多匹配一个 rec）：无迁移 → 丢弃
                let now = rec.task.state.clone();
                let Some((from, raw_to)) = crate::bt_events::transition_for(&now, a) else {
                    break;
                };
                // E30 对齐（PR #72）：失败拦截与轮询路径（ops poll snapshot）同口径——
                // 重试预算未用尽 → 清句柄回 Queued 安排指数退避重激活（调度循环
                // 到期重接入引擎）；预算用尽 → Failed 终态。alert 快路径此前
                // 直写 Failed 绕过重试（轮询兜底路径虽已拦截，但 alert 先到即定终）。
                // 活跃态门控与轮询路径守卫一致（仅 Queued/Downloading 拦截）：
                // Paused/Seeding 下的 Error 保持旧直终语义（暂停任务不得被
                // 重试悄悄复活；做种失败不自动重下）。
                let to = if raw_to == TaskState::Failed
                    && matches!(now, TaskState::Queued | TaskState::Downloading(_))
                {
                    rec.fail_or_schedule_retry(Some(&a.msg))
                } else {
                    raw_to.clone()
                };
                rec.task.state = to.clone();
                // Task 39：做种计时登记/清空（进入 Seeding 登记，离开清空）
                if to == TaskState::Seeding {
                    rec.seeding_since = Some(std::time::Instant::now());
                } else {
                    rec.seeding_since = None;
                }
                if let Some(es) = rec.engine_status.as_mut() {
                    // 错误信息按引擎原始去向记录（重试排队也保留最近失败原因可观测）
                    if raw_to == TaskState::Failed {
                        es.error = Some(a.msg.clone());
                    }
                    // E11：BT 走向非活跃态时轮询缓存仍持最后窗口速率——
                    // 轮询器不再光顾非活跃任务，不清则 /stats 聚合虚高（陈旧速率）。
                    // Seeding 不清：仍是活跃轮询候选，下一轮以引擎实时值刷新。
                    // 重试拦截（Queued）同样清零：引擎已停转，重激活前轮询器
                    // 不再光顾（engine_tid 已清），陈旧速率同理虚高。
                    if matches!(
                        raw_to,
                        TaskState::Paused
                            | TaskState::Completed
                            | TaskState::Failed
                            | TaskState::Stopped
                    ) {
                        es.down_rate = 0;
                        es.up_rate = 0;
                    }
                }
                found = Some(BtAlertEffect {
                    task_id: id.clone(),
                    from,
                    // E30：广播拦截后的实际目标（重试安排 = Queued，非引擎报的
                    // Failed），与轮询路径 E30 注释同口径
                    to,
                    message: a.msg.clone(),
                });
                break;
            }
            found
        }; // ← tasks 锁在此释放（guard drop）
        if effect.is_some() {
            self.autosave(); // 锁外落盘：状态迁移落盘（修复 Bug B 重入自死锁）
        }
        effect
    }
}

/// 从 magnet 提取 btih（40 hex，v1 规范 xt=urn:btih:）。无 → None（canonical 回落全文）。
/// 审计修复（P1）：原实现直接 `split('&')`，而标准磁链首段为 `magnet:?xt=...`，
/// `strip_prefix("xt=urn:btih:")` 对首段永不匹配 → xt 在首位（最常见形态）恒 None
/// → fastresume 回灌整链静默失效 + canonical 查重退化为全文比对。
#[cfg(feature = "bt")]
pub(crate) fn btih_of(magnet: &str) -> Option<String> {
    magnet
        .strip_prefix("magnet:?")
        .unwrap_or(magnet)
        .split('&')
        .find_map(|p| {
            let v = p.strip_prefix("xt=urn:btih:")?;
            (v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| v.to_ascii_lowercase())
        })
}

/// 从 .torrent 字节提取 BT infohash（40 hex 小写）= SHA1(info dict 原始字节)。
/// 只做最小 bencode 定位（顶层 dict 找键 `info` → 配对结束 `e` 取整段），
/// 不做完整解析——足以支撑 canonical 查重。
#[cfg(feature = "bt")]
pub fn torrent_infohash(b: &[u8]) -> Option<String> {
    use sha1::Digest;
    let (info, end) = smart_dl_core::bencode::locate_dict_value(b, b"info")?;
    if b.get(info) != Some(&b'd') {
        return None; // info 必须是 dict
    }
    let digest = sha1::Sha1::digest(&b[info..=end]);
    Some(
        digest
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect::<String>(),
    )
}

/// 单文件 .torrent 总大小（info dict 内 `length` 字段）；多文件（`files`）→ None。
/// v1 仅覆盖单文件场景（B10 空间预检用）；多文件留后续。
/// TD-1：locate + decode 组合（负数 length 语义与旧实现一致 → None）。
#[cfg(feature = "bt")]
pub fn torrent_total_size(b: &[u8]) -> Option<u64> {
    let (info, end) = smart_dl_core::bencode::locate_dict_value(b, b"info")?;
    if b.get(info) != Some(&b'd') {
        return None;
    }
    let v = smart_dl_core::bencode::decode(&b[info..=end]).ok()?;
    if v.dict_get(b"files").is_some() {
        return None; // 多文件：v1 不解析
    }
    v.dict_get(b"length")?
        .as_int()
        .and_then(|n| u64::try_from(n).ok())
}

/// .torrent 空间预检总大小（B10）：优先 TorrentMeta::parse——多文件取 files 各项
/// size 求和、单文件取 file_size；parse 失败 → 回退 torrent_total_size（单文件最小
/// 解析）。两者都拿不到 → None（预检跳过）。
#[cfg(feature = "bt")]
pub fn torrent_precheck_total(b: &[u8]) -> Option<u64> {
    match TorrentMeta::parse(b) {
        Ok(meta) => {
            if meta.files.is_empty() {
                Some(meta.file_size)
            } else {
                Some(meta.files.iter().map(|f| f.size).sum())
            }
        }
        Err(_) => torrent_total_size(b),
    }
}

#[cfg(all(test, feature = "bt"))]
mod btih_tests {
    use super::btih_of;

    const IH: &str = "0123456789abcdef0123456789abcdef01234567";

    /// 审计回归（P1）：xt 在首位的标准磁链必须能提取 btih——原实现对首段
    /// strip_prefix("xt=urn:btih:") 永不匹配（前缀差 "magnet:?"），导致
    /// fastresume 回灌整链静默失效 + canonical 查重退化为全文比对。
    #[test]
    fn btih_xt_first_param() {
        assert_eq!(
            btih_of(&format!("magnet:?xt=urn:btih:{IH}&dn=name")),
            Some(IH.to_string())
        );
    }

    #[test]
    fn btih_xt_later_param() {
        assert_eq!(
            btih_of(&format!(
                "magnet:?dn=name&tr=udp%3A%2F%2Fx&xt=urn:btih:{IH}"
            )),
            Some(IH.to_string())
        );
    }

    #[test]
    fn btih_normalizes_case() {
        assert_eq!(
            btih_of(&format!("magnet:?xt=urn:btih:{}", IH.to_uppercase())),
            Some(IH.to_string())
        );
    }

    #[test]
    fn btih_rejects_base32_and_short() {
        // 32 位 base32（v1 第二表示）非 40-hex，维持既有契约：拒绝（回落全文）
        assert_eq!(btih_of("magnet:?xt=urn:btih:ORSXG5A="), None);
        assert_eq!(btih_of(&format!("magnet:?xt=urn:btih:{IH}Z")), None);
        assert_eq!(btih_of("magnet:?dn=only-name"), None);
    }
}
