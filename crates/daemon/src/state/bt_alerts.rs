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
    pub fn parse(b: &[u8]) -> Result<Self, DaemonError> {
        use sha1::Digest;
        let (info_start, info_end) = locate_info(b).ok_or_else(|| {
            DaemonError::InvalidSource(".torrent 解析失败：无法定位 info dict".into())
        })?;

        let info_hash = {
            let digest = sha1::Sha1::digest(&b[info_start..=info_end]);
            digest
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect::<String>()
        };

        let mut piece_length = 0u32;
        let mut pieces_hash = Vec::new();
        let mut name = String::new();
        let mut file_size = 0u64;
        let mut has_length = false;
        let mut files = Vec::new();
        let mut has_files = false;

        let mut i = info_start + 1; // skip 'd'
        let end = info_end;
        while i < end {
            let (key, after_key) = be_str(b, i)
                .ok_or_else(|| DaemonError::InvalidSource(".torrent info dict 解析失败".into()))?;
            i = after_key;

            match key {
                b"piece length" => {
                    piece_length = be_int(b, i)
                        .ok_or_else(|| DaemonError::InvalidSource("piece length 解析失败".into()))?
                        as u32;
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        DaemonError::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
                b"pieces" => {
                    let pieces_data = be_str(b, i)
                        .ok_or_else(|| DaemonError::InvalidSource("pieces 解析失败".into()))?;
                    if pieces_data.0.len() % 20 != 0 {
                        return Err(DaemonError::InvalidSource(
                            "pieces 长度不是 20 的倍数".into(),
                        ));
                    }
                    pieces_hash = pieces_data.0.as_chunks::<20>().0.to_vec();
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        DaemonError::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
                b"name" => {
                    name = String::from_utf8_lossy(
                        be_str(b, i)
                            .ok_or_else(|| DaemonError::InvalidSource("name 解析失败".into()))?
                            .0,
                    )
                    .into_owned();
                    // 安全修复（V3）：torrent 根名直通 dest_root.join，恶意 name
                    // （../、绝对路径）即任意文件写——parse 层即拒任务。
                    smart_dl_core::session::output::sanitize_rel(&name).map_err(|_| {
                        DaemonError::InvalidSource(format!(
                            ".torrent name 含非法路径分量已拒绝: {name}"
                        ))
                    })?;
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        DaemonError::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
                b"length" => {
                    file_size = be_int(b, i)
                        .ok_or_else(|| DaemonError::InvalidSource("length 解析失败".into()))?
                        as u64;
                    has_length = true;
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        DaemonError::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
                b"files" => {
                    has_files = true;
                    // files value 是 list（l...e）
                    let list_end = list_skip(b, i, 0)
                        .ok_or_else(|| DaemonError::InvalidSource("files 解析失败".into()))?;
                    files = parse_file_list(&b[i + 1..list_end], piece_length)?;
                    i = list_end + 1; // 跳过 list 的闭合 'e'
                }
                _ => {
                    i = value_skip(b, i, 0).ok_or_else(|| {
                        DaemonError::InvalidSource(".torrent info dict 解析失败".into())
                    })?;
                }
            }
        }

        if piece_length == 0 || pieces_hash.is_empty() {
            return Err(DaemonError::InvalidSource(
                ".torrent 缺少必要字段 (piece length/pieces)".into(),
            ));
        }

        if has_files {
            // 多文件 torrent：files 数组已解析
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
            if !has_length {
                return Err(DaemonError::InvalidSource(
                    ".torrent 缺少 length 字段".into(),
                ));
            }
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

/// 解析多文件 torrent 的 files 列表内容（bencode，`l`/`e` 已剥离）。
#[cfg(feature = "bt")]
fn parse_file_list(data: &[u8], piece_length: u32) -> Result<Vec<FileMeta>, DaemonError> {
    let mut files = Vec::new();
    let mut pos = 0usize;
    let plen = piece_length as u64;

    while pos < data.len() {
        if data.get(pos) != Some(&b'd') {
            pos = value_skip(data, pos, 0)
                .ok_or_else(|| DaemonError::InvalidSource("files 列表解析失败".into()))?;
            continue;
        }
        let dict_end = dict_skip(data, pos, 0)
            .ok_or_else(|| DaemonError::InvalidSource("files dict 解析失败".into()))?;
        let file_dict = &data[pos..=dict_end];

        let mut path = String::new();
        let mut length = 0u64;
        let mut j = 1;
        while j < file_dict.len() - 1 {
            let (key, after_key) = be_str(file_dict, j)
                .ok_or_else(|| DaemonError::InvalidSource("files dict key 解析失败".into()))?;
            j = after_key;
            match key {
                b"length" => {
                    length = be_int(file_dict, j)
                        .ok_or_else(|| DaemonError::InvalidSource("files length 解析失败".into()))?
                        as u64;
                    j = value_skip(file_dict, j, 0).ok_or_else(|| {
                        DaemonError::InvalidSource("files dict value 解析失败".into())
                    })?;
                }
                b"path" => {
                    // path value 是 list（l...e）
                    let path_list_end = list_skip(file_dict, j, 0).ok_or_else(|| {
                        DaemonError::InvalidSource("files path list 解析失败".into())
                    })?;
                    path = parse_path_list(&file_dict[j + 1..path_list_end])?;
                    j = path_list_end + 1;
                }
                _ => {
                    j = value_skip(file_dict, j, 0).ok_or_else(|| {
                        DaemonError::InvalidSource("files dict value 解析失败".into())
                    })?;
                }
            }
        }

        if length > 0 && !path.is_empty() {
            // 计算 piece 偏移和数量（按文件在 torrent 中的累计字节偏移）
            let total_size: u64 = files.iter().map(|f: &FileMeta| f.size).sum();
            let piece_offset = (total_size / plen) as usize;
            let piece_count = length.div_ceil(plen) as usize;
            files.push(FileMeta {
                path,
                size: length,
                piece_offset,
                piece_count,
            });
        }

        pos = dict_end + 1;
    }

    Ok(files)
}

/// 解析 path list 内容（bencode，`l`/`e` 已剥离）为路径字符串。
/// 安全修复（V3）：逐段净化——拒 `..` / 绝对路径段，恶意种子不得写出 dest_root。
#[cfg(feature = "bt")]
fn parse_path_list(data: &[u8]) -> Result<String, DaemonError> {
    let mut parts = Vec::new();
    let mut p = 0usize;
    while p < data.len() {
        let (seg, after) = be_str(data, p)
            .ok_or_else(|| DaemonError::InvalidSource("path segment 解析失败".into()))?;
        let seg_str = String::from_utf8_lossy(seg).into_owned();
        if seg_str == ".." || seg_str.contains('/') || seg_str.contains('\\') || seg_str.is_empty()
        {
            return Err(DaemonError::InvalidSource(format!(
                "files path 含非法段已拒绝: {seg_str}"
            )));
        }
        parts.push(seg_str);
        p = after;
    }
    if parts.is_empty() {
        return Err(DaemonError::InvalidSource("files path 为空".into()));
    }
    Ok(parts.join(std::path::MAIN_SEPARATOR_STR))
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
#[cfg(feature = "bt")]
pub(crate) fn btih_of(magnet: &str) -> Option<String> {
    magnet.split('&').find_map(|p| {
        let v = p.strip_prefix("xt=urn:btih:")?;
        (v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit())).then(|| v.to_ascii_lowercase())
    })
}

/// 从 .torrent 字节提取 BT infohash（40 hex 小写）= SHA1(info dict 原始字节)。
/// 只做最小 bencode 定位（顶层 dict 找键 `info` → 配对结束 `e` 取整段），
/// 不做完整解析——足以支撑 canonical 查重。
#[cfg(feature = "bt")]
pub fn torrent_infohash(b: &[u8]) -> Option<String> {
    use sha1::Digest;
    let (info, end) = locate_info(b)?;
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
#[cfg(feature = "bt")]
pub fn torrent_total_size(b: &[u8]) -> Option<u64> {
    let (info, end) = locate_info(b)?;
    let mut i = info + 1;
    while i < end {
        let (key, ai) = be_str(b, i)?;
        i = ai;
        match key {
            b"length" => {
                if b.get(i) != Some(&b'i') {
                    return None;
                }
                let e = b[i..].iter().position(|&c| c == b'e')? + i;
                return std::str::from_utf8(&b[i + 1..e]).ok()?.parse().ok();
            }
            b"files" => return None, // 多文件：v1 不解析
            _ => i = value_skip(b, i, 0)?,
        }
    }
    None
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

/// 定位 info dict：返回 (info 值起始 'd' 下标, info dict 闭合 'e' 下标)。
#[cfg(feature = "bt")]
fn locate_info(b: &[u8]) -> Option<(usize, usize)> {
    if b.first() != Some(&b'd') {
        return None;
    }
    let mut i = 1; // 顶层 dict 键值对扫描
    while i < b.len() {
        let (key, after_key) = be_str(b, i)?;
        i = after_key;
        if key == b"info" {
            if b.get(i) != Some(&b'd') {
                return None; // info 必须是 dict
            }
            let end = dict_skip(b, i, 0)?;
            return Some((i, end));
        }
        // 跳过值（结构感知），继续找 `info`
        i = value_skip(b, i, 0)?;
    }
    None
}

/// bencode 字符串 `len:data` → (data, 内容后下标)。
#[cfg(feature = "bt")]
fn be_str(b: &[u8], at: usize) -> Option<(&[u8], usize)> {
    let colon = b[at..].iter().position(|&c| c == b':')? + at;
    let len: usize = std::str::from_utf8(&b[at..colon]).ok()?.parse().ok()?;
    let start = colon + 1;
    // 安全修复（H-3 同型）：start+len 裸加法——恶意 fastresume/torrent 的超大
    // 长度字段在 release 下回绕或直接越界 → 切片 panic。checked_add + 界检查。
    let end = start.checked_add(len)?;
    if end > b.len() {
        return None;
    }
    Some((&b[start..end], end))
}

/// bencode 整数 `i<digits>e` → 值。
#[cfg(feature = "bt")]
fn be_int(b: &[u8], at: usize) -> Option<i64> {
    if b.get(at) != Some(&b'i') {
        return None;
    }
    let e = b[at..].iter().position(|&c| c == b'e')? + at;
    let s = std::str::from_utf8(&b[at + 1..e]).ok()?;
    s.parse().ok()
}

/// dict 结束下标：从 `start`（'d'）按 键(字符串)→值 结构推进到闭合 'e'。
/// 键位置固定为字符串（len: 数字开头），值可为任意类型——值内的数据字节
/// （如 pieces 的 20 字节）不会被误当 len: 解析。
/// 安全修复（V4）：带深度参数，超限返回 None（恶意种子不再能栈溢出 abort）。
#[cfg(feature = "bt")]
fn dict_skip(b: &[u8], start: usize, depth: usize) -> Option<usize> {
    const MAX_DEPTH: usize = 64;
    if depth > MAX_DEPTH {
        return None;
    }
    let mut i = start + 1;
    while b.get(i) != Some(&b'e') {
        let (_, after) = be_str(b, i)?; // 键：字符串
        i = value_skip(b, after, depth + 1)?; // 值：任意类型
    }
    Some(i)
}

/// list 结束下标：从 `start`（'l'）按 值* 推进到闭合 'e'。
#[cfg(feature = "bt")]
fn list_skip(b: &[u8], start: usize, depth: usize) -> Option<usize> {
    let mut i = start + 1;
    while b.get(i) != Some(&b'e') {
        i = value_skip(b, i, depth + 1)?;
    }
    Some(i)
}

/// 跳过任意 bencode 值（dict/list/int/str），返回其后的下标。
#[cfg(feature = "bt")]
fn value_skip(b: &[u8], i: usize, depth: usize) -> Option<usize> {
    match b.get(i)? {
        b'd' => dict_skip(b, i, depth).map(|e| e + 1),
        b'l' => list_skip(b, i, depth).map(|e| e + 1),
        b'i' => {
            let e = b[i..].iter().position(|&c| c == b'e')? + i;
            Some(e + 1)
        }
        _ => be_str(b, i).map(|(_, after)| after),
    }
}
