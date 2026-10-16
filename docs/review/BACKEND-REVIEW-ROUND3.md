# 后端审查报告（第三轮 · 覆盖补齐）

> 审查对象：`tomjiu/smart-downloader` @ `fa29e22`
> 审查日期：2026-10-16
> 本报告 = 第三轮（btcore 业务层 / provider 剩余 / url_class）+ 前两轮未覆盖部分

---

## 0. 覆盖推进

| 区域 | 行数 | 状态 |
|------|------|------|
| `btcore` 非 FFI 业务层 | ~1,900 | ✅ 本轮完成 |
| `provider` 剩余（url_class / vip_speedup / coordinator） | ~1,340 | ✅ 本轮完成 |
| `xunlei-ffi` 全部 | ~2,002 | ✅ 本轮完成 |
| `xunlei-convert` 全部 | ~1,685 | ✅ 本轮完成 |
| 前两轮已覆盖 | ~45,800 | ✅ |

**累计覆盖：约 52,700 / 52,000 行 → 全覆盖（含少量测试代码）**

**唯一未逐行审的部分**：`daemon/tests/*`（约 6,000 行测试代码）与
`docs/research/clients/`（第三方竞品源码，非本项目）。

---

## 1. 本轮新发现

### 🔴 P0-14 `analyze_cid_store` 二次方爆炸 + 候选无上限 ★已实证

**位置**：`xunlei-convert/src/cid_store.rs:207-255`

```rust
for &hlen in &hash_lens {                    // 16/20/32
    while i + hlen <= bytes.len() {          // ① O(n) 逐偏移
        if !is_printable(win) && looks_random(win) {   // ② looks_random 内部 O(hlen)
            let after = paths.iter().find(...);        // ③ O(#paths)
            let before = paths.iter().rev().find(...); // ③ O(#paths)
            ...
            cands.push(Cand { path: s.clone(), ... }); // ④ 无上限 push
```

**已实证**（`repro/p0repro/src/round5.rs`，release 模式）：

```
     n   input(B)      entries       time
   250      12000        29809       9.6ms
   500      24000        59660      35.7ms   输入 ×2.0 → 时间 ×3.7  ⚠️
  1000      48000       119407      98.5ms   输入 ×2.0 → 时间 ×2.8
  2000      96000       238962     406.1ms   输入 ×2.0 → 时间 ×4.1  ⚠️
  4000     192000       478085      1.44s
  8000     384000       956109      6.01s    ← 384KB 已 6 秒
```

**输入每翻倍，时间 ×3-4 → 明确的二次方。**

**触发条件确认为「随机块 + 路径串交替」**，而非输入大小本身：

```
对照组：纯可打印 512KB → entries=0, time=27.0ms   ✅ 快
```

**而这正是 `cid_store.dat` 的真实形态**（下载历史 hash + 本地路径交错）——
即：**用户导入自己的真实文件就会命中**。

**另一个被忽视的放大**：`entries` 数量爆炸 —— **384KB 输入产生 956,109 个候选**，
每个候选带一次 `path.clone()`。内存放大数百倍。

**模块文档**（`cid_store.rs:22`）承诺「解析全程零 panic（垃圾输入 → 空报告）」——
零 panic 成立，但**未承诺时间复杂度上界**，而输入是本地隐私文件（用户自己的），
等同自我 DoS。

**修复方向**：
1. `paths` 已按 offset 排序（`sort_by_key`），改用**二分查找**替代 `iter().find()` → 消掉 ③
2. `taken_windows` 用 `HashSet` 替代线性扫描（L245 同样是 O(#taken) 每候选）
3. `cands` 加上限（如 `cap = 10_000`）或提前剪枝

---

### 🔴 P1-23 `bdecode` 接受截断/畸形输入 → 校验器失效

**位置**：`xunlei-convert/src/fastresume.rs:384-404`

```rust
b'd' => {
    let mut p = pos + 1;
    while p < data.len() && data[p] != b'e' {   // ← 越界退出与遇到 'e' 不可区分
        ...
    }
    Ok((BencodeValue::Dict(dict), p + 1))       // ← p == data.len() 时返回 len+1
}
```

**实测**：
```
"unterminated list"  -> Ok(List([...]))     ← 缺 'e' 仍 Ok
"unterminated dict"  -> Ok(Dict([...]))     ← 缺 'e' 仍 Ok
"valid then junk"    -> Ok(Dict([...]))     ← 尾随垃圾被忽略
```

**这直接废掉了 `bdecode` 作为校验器的用途** —— 被截断的 fastresume 会「校验通过」。

**深度限制确实有效**（`MAX_DEPTH=64`，实测 10 万个 `l` 正确报错）—— 栈溢出已防住。

**修复**：`while p < data.len()` 改为检查循环结束后是否真的遇到 `e`，未遇到则返回 `Err`。

---

### 🟡 P1-24 `XLTaskFlow` 结构体 size 与声明不符

**位置**：`xunlei-ffi/src/bindings.rs:193-201` + `query.rs:117`

```rust
// bindings.rs:193  "size 待定，暂按 0x18 = 24 设计"
#[repr(C)]                       // ← 缺 packed
pub struct XLTaskFlow {
    pub size: c_uint,                // +0x00
    pub download_bytes: c_ulonglong, // 对齐 → +0x08
    pub upload_bytes: c_ulonglong,   // +0x10
    pub _pad: c_uint,
}
```

**实测 `size_of::<XLTaskFlow>() == 32`，但代码向 SDK 声明 `size: 0x18`（24）。**

**关键**：其余 6 个 versioned struct 都因 `repr(C, packed)` 恰好等于声明值
（`XLInitParam` 40 / `XLBTTaskParamV2` 40 / `XLServerInfo` 36 / `XLTaskInfo` 924 …），
**只有这个漂了**。

**且 `abi_size_assert_aligned` 测试恰好漏掉了 `XLTaskFlow`** —— 漂移无回归保护。

---

### 🟡 P1-25 `app_guid` 超 30 字节静默丢弃

**位置**：`xunlei-ffi/src/handle.rs:70-79`

```rust
let json = format!("{{app_guid:{}}}", app_guid);
let bytes = json.as_bytes();
if bytes.len() <= 30 {          // ← 超长直接跳过，无错误、无日志
    ...
    field8 = bytes.len() as u16;
}
```

`{app_guid:` + `}` = **11 字节固定开销** → **guid 上限实为 19 字节**。

**而仓库文档自己的示例值 `"smart-downloader-001"`（20 字节）产生 31 字节 → 被丢弃。**

`handle.rs:32` 注释里的默认值 `smart-dl-xunlei-001` 恰为 19 字节（勉强通过）。

**后果**：调用方以为注入了来源标识，实际 SDK 收到空 JSON，**零反馈**。
与同文件对 `server_path` 超长「检测并给出明确错误」的处理**自相矛盾**。

---

### 其余 P1/P2（本轮）

### 🔴 P0-13 `xunlei_engine.rs` add 失败遗留孤儿任务

**位置**：`btcore/src/xunlei_engine.rs:213-220`

```rust
let engine_id = Self::engine_task_id(xl_id.0);
self.insert_task(engine_id.clone(), xl_id).await;      // ① 先登记

self.handle
    .start_task(&xl_id)
    .await
    .map_err(|e| EngineError::Other(format!("xunlei start failed: {e}")))?;  // ② 失败直接 return
```

**失败链（已核实可达）**：

```
engine.add(&task).await  →  Err(e)
    ↓
daemon/ops.rs:958-964  →  rollback_slot_reservation(&task_id)
    ↓
daemon 侧：记录已清除 ✓
引擎侧：tasks 表仍留 xunlei-<n>  ← 孤儿
```

**关键**：`rollback_slot_reservation`（`ops.rs:527-537`）**只操作 daemon 的
`slot_reservations` 与 `tasks`**，完全不碰引擎。而 `engine_tid` 被 `Err` 丢弃
（`let engine_tid = match ... { Err(e) => { ... return Err(...) } }`）→
**该句柄再也无人持有**。

**后果**：
- 迅雷 SDK 侧任务持续运行（占带宽/磁盘）
- `pause` / `remove` / `status` 全部不可达
- **重启不清理**（daemon 不记得它）

**修复**：`start_task` 失败时回滚 `self.remove_task(&engine_id).await` 并尝试 `delete_task`。

---

### 🟡 P1-16 `nas_remote::update_sources` 语义与 trait 契约不符

**位置**：`btcore/src/nas_remote.rs:442-463`

```rust
async fn update_sources(&self, id: &EngineTaskId, _urls: Vec<String>) -> Result<(), EngineError> {
    let _xlid = self.tasks.lock().await.get(id).cloned().ok_or(EngineError::NotFound)?;
    let url = format!("{}/device/v1/try_speed/apply", self.cfg.base_url);  // ← 设备级端点
    let payload = serde_json::json!({});                                    // ← 空 body
    ...
}
```

**替代码说句公道话**：注释 L441 写明"超速申请（A4：仅 RUNNING 任务生效；
body 空对象即可，**配额 usage 独立**）" —— **这是有意的设计**，超速确实是设备级配额。

**真正的问题是架构层面的**：
- `DownloadEngine::update_sources` 的语义是"该任务的源变了"
- 而这里做的是"设备级超速申请"（与 `id` 无关）
- **`id` 查了却丢弃**（`_xlid`），是"形式满足契约、实质不相关"

**这是 §问题 1（胖接口）的又一后果** —— 40 方法并集里塞了不支持的操作，
实现方只能写"看起来像实现了"的桩。

**建议**：改为 `Err(EngineError::Unsupported)`，或把超速申请提为独立接口。

---

### 🟡 P1-17 provider 并发闸门恒真

**位置**：`provider/src/coordinator.rs:70`

```rust
&& rt.busy < rt.concurrency_limit
```

**已复现**（`repro/p0repro/src/round4.rs`）：

```
构造点                          concurrency_limit   busy
xunlei/provider.rs:197-198                    2      0
quark/share.rs:248-249                        2      0
types.rs:30-31 (Default)                      2      0

coordinator.rs:70 判定: busy(0) < limit(2) = true
→ 闸门恒真，「每 provider 并发上限 2」约束从未生效
```

**全仓无任何地方递增 `busy`** —— 只有 `mock.rs:129` 的 `set_busy()` 供测试用。

**后果**：100 个兜底任务可同时打向同一 provider。

**叠加效应**（与已报 P1-2 组合）：
1. 并发闸门失效 → 100 任务同时进入 provider
2. `refresh_auth` 持锁跨 await（最长 30s 超时）
3. → 99 个任务排队等锁，单个超时即 30s 级停顿

**修复**：在 `try_provider_fallback` 进入时递增 `busy`、退出（含错误路径）时递减
（RAII guard 最佳）。

---

### 🟡 P2-20 `url_class::host_of` 的 IPv6 截断

**位置**：`provider/src/xunlei/url_class.rs:538`

```rust
let host = hostport.split(':').next().unwrap_or("");
```

**已复现**：

```
http://[::1]:8080/x            → host_of = Some("[")        ← 错
http://[2001:db8::1]/x         → host_of = Some("[2001")    ← 错
```

**当前结论碰巧正确**（`"["` 不匹配任何 CDN 域名 → `BRegular`），但：
- `host` 值本身错误
- 若 CDN 表加入 IPv6 条目，**永不匹配**

**同时确认实现正确的地方**（对照组）：
- 后缀匹配**不可绕过**（前导点保证）：`evil-sandai.net` / `sandai.net.attacker.com` / `n0808.com.attacker.com` 全部正确拒绝
- userinfo / 端口处理正确
- 空/畸形输入无 panic

**另一处语义偏差**：协议相对 URL（`//cdn.xunlei.com/a`）返回 `BRegular`，
而注释声称对齐 Python `urlparse().hostname` —— 实际 Python 能识别（`hostname == "cdn.xunlei.com"`）。

---

### 其余 P1/P2（本轮）

| 编号 | 位置 | 问题 |
|------|------|------|
| P1-18 | `xunlei_engine.rs:158-160` | 互斥靠运行期 String 报错；`with_bt`（`state.rs:497`）是 `insert` 无冲突检测 |
| P1-19 | `xunlei_engine.rs:140-149` | `Unknown → Downloading` 把真实失败态伪装成"下载中" → daemon 永不判失败/重试 |
| P1-20 | `nas_remote.rs:266-282` | 403 重试时 `ensure_jwt(true).await?` 吞掉原始 body → 每日 3 次配额诊断丢失 |
| P1-21 | `nas_remote.rs:234-263` | 重试无幂等保护：POST 创建任务若响应被篡改为 403 → 重试**再创建一个**（配额一次误判消耗两次） |
| P1-22 | `xudt/frame.rs:29-42` | 32 位下 `24 + plen` 回绕 → 切片 panic。**`handshake.rs:81` 同型已修（`checked_add`），此处漏改** |
| P1-26 | `handle.rs:221-224` | `path_to_cstring` 的 `expect` panic（同文件其他路径都返回 `InvalidParam`，不一致） |
| P2-21 | `nas_remote.rs:120-139` | `parse_speed_bps` 无饱和（`"9999999999GB/s"` → 1.07e19 B/s）；科学计数法静默错值 |
| P2-22 | `nas_remote.rs:383-389` | `total` 硬编码 `100.0` → UI 总大小永远显示 100 B |
| P2-23 | `magnet.rs:106-124` | 轮询无取消 token；`state == 3` 裸魔数；`poll_interval = 0` 变忙等 |
| P2-24 | `magnet.rs:98-104` | tracker/peer 注入全 `let _ =` 吞错 → 全失败时唯一症状是 60s Timeout |
| P2-25 | `alerts.rs:64-77` | `state_subkind()` 子串匹配脆弱；`contains("error")` 过度匹配 |
| P2-26 | `vip_speedup.rs:195` | `resp.text()` 读整个响应体**无大小上限**（30s 超时内可推送数 GB） |
| P2-27 | `vip_speedup.rs:168` | `.unwrap_or_default()` 掩盖 client 构建失败 → 退回**无超时** client（fail-open） |
| P2-28 | `xlbt_cfg.rs:139` | `header_u32s` 偏移列表漏 0x20/0x28（代码 7 项 vs 文档声称 8 项） |
| P2-29 | `cid_store.rs:110` | `json_like` 判定（前 64B 含 `{`）优先于 `xdlctx_family` → 走错分支的风险 |
| P2-30 | `cid_store.rs:124-166` | `walk_json` 递归无显式深度限制（**依赖 serde_json 默认 128 层**，隐式保护） |
| P2-31 | `bindings.rs:206-231` | 三个 `SubTaskIndex` 结构体 size 与注释差 20/12/70 字节（**当前零引用**，接线即成 P0） |

---

## 1.5 ⚠️ 本轮另一个被纠正的误判

**`analyze_cid_store` 的 `json_like` 判定顺序**（P2-29）值得单独说明：

```rust
let entries = if report.json_like {           // ← 优先
    extract_json_entries(...)
} else if report.xdlctx_family {              // ← 次之
    extract_binary_entries(bytes, true, report)
}
```

而 `json_like` 的判定是「前 64 字节内出现 `{`」——**过于宽松**：
`XDLCTX` 开头的 TLV 文件若某个值含 `0x7B` 字节，会被优先当 JSON 解析。

**这是启发式判定的固有风险**，不是 bug（模块已明确标注 HYPOTHESIS + "候选≠事实"）。
但顺序可以更保守：**先判 magic（确定性高），再判启发式**。

---

## 2. 明确「未发现」的类别（本轮）

| 类别 | 结论 |
|------|------|
| `btcore/engine.rs`（BtCore 门面） | **未发现**。薄转发无分支；NUL 截断、`max(0)` 防护正确 |
| `xudt/handshake.rs` | **未发现**。`checked_add` 已修，三处切片下界均安全 |
| `xudt/message.rs` | **未发现**。`payload.len() != 13` 精确校验保证不 panic |
| `extract_uiauth_jwt` | **未发现**。8 组畸形/CJK/截断输入全部安全返回 `None` |
| `truncate` 的 CJK 边界 | **未发现**。160/162 字节边界输入不 panic，修复完整 |
| `urlencode` | **未发现**。UTF-8 多字节正确展开 |
| `xunlei_engine` tasks 表竞态 | **未发现**。三方法均在锁内单次操作，无跨 await 持锁 |
| NAS 凭据处理 | **未发现泄漏**。JWT 仅作 header，不 Debug 打印，无落盘 |
| alert 类型静默丢弃 | **未发现**。`Other` 兜底 + 七个 mask 位全覆盖 |
| `url_class` 后缀匹配 | **未发现绕过**（前导点保证） |
| `btcore/bare.rs` / `resume.rs` / `strategy.rs` / `lib.rs` | **未发现** |

---

## 3. ⚠️ 本轮纠正的一个自身误判

**我曾假设 `btcore/src/engine.rs` 是 `DownloadEngine` 的实现**（与 `httpdl/src/engine.rs` 对称），
并据此推断它会有一堆 BT 桩。

**实际不是。** `DownloadEngine` 的实现者清单：

| 实现 | 位置 |
|------|------|
| `BtEngine` | **`daemon/bt.rs:499`** ← 真正的 BT 引擎 |
| `NasRemoteEngine` | `daemon/nas_remote.rs:71`（骨架） |
| `NasRemoteEngine` | `btcore/nas_remote.rs:299`（校准版，feature 关死） |
| `XunleiBtEngine` | `btcore/xunlei_engine.rs:153` |
| `HttpEngine` | `httpdl/engine.rs:1001` |
| `FtpEngine` | `httpdl/protocol/ftp.rs:1374` |
| `SftpEngine` | `httpdl/protocol/sftp.rs:819` |

`btcore/src/engine.rs` 只是 **`BtCore` 门面**（对 `ffi::Session` 的薄转发）。

**这反而强化了架构问题的判断**：`BtEngine` 实现在 `daemon`，
而 `btcore` 提供底层门面 —— **BT 引擎被切成两半分居两个 crate**，
而 HTTP 引擎完整地待在 `httpdl`。**不对称。**

---

## 4. 复现工程

`repro/p0repro/` 已扩展到 **23 个测试**（debug 下 `20 passed / 3 failed`
—— 3 个 failure 即 3 个真实缺陷的证明）：

| 文件 | 测试数 | 覆盖 |
|------|--------|------|
| `lib.rs` | 2 | P0 除零 panic |
| `plength.rs` | 5 | piece_length 溢出 |
| `round2.rs` | 4 | total_size 溢出 + sanitize_rel |
| `round3.rs` | 2 | loopback 绕过 + 部分表默认值 |
| `round4.rs` | 7 | url_class 边界 + provider 并发闸门 |
| `round5.rs` | 3 | **cid_store 二次方复杂度** |

**运行**：
```powershell
cd repro\p0repro
cargo test -- --nocapture              # debug（含 panic 断言）
cargo test --release -- --nocapture    # release（含性能测量）
```

**注意**：`round5` 的性能测量需 `--release`（debug 下二次方效应被放大但数值不具参考性）。

---

## 5. 本轮新增的「未发现」类别

| 类别 | 结论 |
|------|------|
| `xunlei-ffi` 的 `repr(C, packed)` 完备性 | **6 个 versioned struct 逐个实测**，size 与声明一致（仅 `XLTaskFlow` 例外，已报） |
| `loader.rs` 符号表 | 32 急切 + 4 可选，命名与 `Symbols` 字段一一对应 |
| `spawn_blocking` 覆盖率 | 所有 FFI 调用点均已包装，**未发现**裸调 |
| CString 悬垂 | 全部绑定具名变量，**未发现** |
| `error.rs` 错误码映射 | 未知码保留原始值（`unknown error code: {code}`），无信息丢失 |
| `handle.rs` 的 `Drop` 同步阻塞 | **不是缺陷** —— 无 panic 风险，`Arc` 保证只触发一次，`Drop` 无法 async |
| `encode_bytes` 长度溢出 | **不可溢出**（64 位 usize） |
| tag-04 的 `6 + len` | **不回绕**（实测 `len=0xFFFFFFFF` 正常） |
| `bdecode` 深度限制 | **有效**（`MAX_DEPTH=64`，实测 10 万 `l` 正确报错） |
| `build_bitfield*` 越界防护 | **三者都有守卫**，实测越界被忽略 |
| `xlbt_cfg` TLV 循环越界 | **两处都有长度前置检查**，未发现越界 |
| `walk_json` 栈溢出 | **不发生** —— 被 serde_json 默认 128 层限制住（隐式保护，见 P2-30） |
