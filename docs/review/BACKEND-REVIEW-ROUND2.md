# 后端全面体检报告（第二轮 · 覆盖补齐）

> 审查对象：`tomjiu/smart-downloader` @ `0520e25`
> 审查日期：2026-10-16
> 本报告 = 第一轮（daemon 核心 + httpdl）+ 第二轮（provider / daemon 未覆盖 / FFI 手工核）

---

## 0. 覆盖矩阵

| crate | 行数 | 第一轮 | 第二轮 | 覆盖率 |
|-------|------|--------|--------|--------|
| httpdl | 9,059 | ✅ 全部 | — | **100%** |
| daemon | 23,195 | ✅ state/http/bt/rss/serve | ✅ 本轮补完其余 | **~100%** |
| provider | 8,291 | ❌ | ✅ 全部 26 文件逐行 | **100%** |
| core | 5,311 | ❌ | ✅ 全部 29 文件 | **100%** |
| btcore | 2,700 | ❌ | ✅ FFI 层手工核 | 部分 |
| xunlei-ffi | 2,002 | ❌ | ✅ FFI 层手工核 | 部分 |
| xunlei-convert | 1,685 | ❌ | ✅ fastresume/xltd 核 | 部分 |

**方法说明**：core 与 provider 两路均采用「实测验证」—— 写临时 probe 跑出实际行为，
而非仅静态阅读。probe 已清理，工作树无残留。

---

## 1. 全部 P0 汇总（12 个）

> **实证状态**：标记 ★ 的项已在 `repro/p0repro` 中跑出真实结果（非静态推断）。

| # | 问题 | 位置 | 实证 |
|---|------|------|------|
| P0-1 | 多文件 `.torrent` 除零 panic | `daemon/state/bt_alerts.rs:117+215` | ★ panic |
| P0-2 | `pause_intents` guard 跨 FFI | `daemon/bt.rs:236-243` | 代码确认 |
| P0-3 | `poll_ring` 互吞 alert | `daemon/bt.rs:395-408` | 代码确认 |
| P0-4 | 无摘要+预分配 = 空洞文件 | `httpdl/download.rs:111` + `engine.rs:739` | 代码确认 |
| P0-5 | 双源无 ETag 跨源混拼 | `httpdl/engine.rs:1632` | 代码确认 |
| P0-6 | 匿名设备号不匹配 | `provider/xunlei/share.rs:225/437/501` | 代码确认 |
| P0-7 | 登录页无 Host/Origin 校验 | `provider/xunlei/login_page.rs:78-103` | ★ 量化 0 命中 |
| P0-8 | 夸克任务 ID 覆盖 | `provider/quark/share.rs:215` | 代码确认 |
| P0-9 | 迅雷任务 ID 秒级碰撞 | `provider/xunlei/provider.rs:250` | 代码确认 |
| P0-10 | `PUT /settings` 触发 exit/关机 | `daemon/state/lifecycle.rs:82` | 代码确认 |
| P0-11 | `total_size` 溢出绕过磁盘预检 | `core/torrent_meta.rs:316` | ★ 回绕 |
| P0-12 | core 侧 `piece_len` 不校验 | `core/torrent_meta.rs:274` | ★ 0/-5 被接受 |

---

### 🔴 P0-11 `total_size` 求和溢出 → 磁盘预检被绕过 ★

**`core/src/torrent_meta.rs:316`**

```rust
// L309  size 来自 `size.max(0) as u64`，无上界校验
size: size.max(0) as u64,
// L316  无 checked_add
let total_size = files.iter().map(|f| f.size).sum();
```

**已实证**（`repro/p0repro/src/round2.rs`）：

```
每文件 size  = 9223372036854775807 (i64::MAX)
文件数        = 3
真实总和     = 27670116110564327421
超过 u64::MAX = true
→ 原实现 (release 回绕) = 9223372036854775805
→ 回绕后变小 → precheck_space 误判磁盘充足 → 放行入队
```

**边界情形**：`[u64::MAX, 1]` → 回绕为 **`0`** → 声称总大小 0 字节 → 预检必然放行。

**下游**：`http.rs:1237` 透出 → `ops.rs:1149 precheck_space` → `evaluate_disk`。
这是 **D36 磁盘防线的整数层绕过**。

**修复**：`try_fold(0u64, |a, f| a.checked_add(f.size))`，溢出报错。

---

### 🔴 P0-12 core 侧 `piece_len` 不校验（daemon P0-1 的同源缺口）

**`core/src/torrent_meta.rs:274` + `:238`**

**好消息**：core 的 `num_pieces` 用 `pieces.len() / 20`，**不是** `ceil(total / piece_len)` → **无除零**。

**坏消息**：`piece_len` **只校验类型不校验取值**。实测 `piece length = 0` 和 `-5` **均被 ACCEPTED**。

**影响**：core 的 `parse_torrent` 是公开 API（"任务预览/子文件选择"门面），
任何**绕过 daemon parse 直接用 core 摘要**的调用方会踩到除零。

**另**：`pieces.len() % 20 != 0` 时余数被**静默丢弃**（21 字节 → `num_pieces = 1`），
而 daemon 侧明确报错 → **两道防线口径不一致**。

---

### 🔴 P0-1 多文件 `.torrent` 除零 panic ★已实证复现

**`daemon/src/state/bt_alerts.rs:117` + `:215`**

bencode 字典序要求 `files < name < piece length < pieces`，扫到 `files` 时 `piece_length` 仍为 0 → `total_size / plen` 除零。

**已复现**（`repro/p0repro`）：
```
对照组（现有测试的非法键序）  → ok
真实键序多文件 torrent        → panicked: attempt to divide by zero
```

**测试盲区**：`torrent_tests.rs:90-112` 手工把 `piece length` 写在 `files` 之前（**非法键序**），故恒绿。

**修复**：延后偏移计算 + 补字典序 fixture。

---

### 🔴 P0-2 `pause_intents` guard 跨 FFI

**`daemon/src/bt.rs:236-243`** — guard 活着时调 `core.status()`（libtorrent FFI），500ms 心跳线程抢同锁 → 全任务 fastresume 3s 超时。

**修复**：先取 status 再拿锁（两行）。

---

### 🔴 P0-3 `poll_ring` 互吞 alert

**`daemon/src/bt.rs:395-408`** — `poll_ring` 循环外一次性采样，窗口内 `pop_alerts(256)` 消费整 ring 但只处理 `Resume` → 其它任务永久卡 `Downloading`。

**根因**：`bt_events.rs:52-54` 把 `mark_alert_loop_active()` 包在 `if let Some(g)` 里。

---

### 🔴 P0-4 无摘要 + 预分配 = 静默交付空洞文件

**`httpdl/src/download.rs:111` + `engine.rs:739`** — `set_len(total)` 让长度检查失效，无摘要时 `verify = Ok(true)`。**FTP/SFTP 全文无内容校验**。

---

### 🔴 P0-5 双源无 ETag 时跨源混拼

**`httpdl/src/engine.rs:1632`** — `etag_changed` 要求双方都 `Some`，主流无 ETag CDN 上恒 false → 用旧源账本续传新源。

---

### 🔴 P0-6 匿名设备号每次重生成 → captcha_token 永不匹配

**`provider/src/xunlei/share.rs:225 / 437 / 501`**

```rust
fn anonymous_device_id() -> String {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();  // ← 每次新值
    ...
}
```

三个调用点**各自独立调用**：
- `ensure_captcha()` 用设备号 A 取 `captcha_token` 并缓存
- `list()` 用设备号 B 组装 `x-device-id` → **token 与 device 不匹配**
- `resolve()` 用设备号 C

**L439 注释写"与 captcha/init 一致"——与实际相反。**

**影响**：匿名分享解析 **100% 失败**。

**修复**：设备号缓存进 `Sharer`，`ensure_captcha` 返回 `(token, did32)`。

---

### 🔴 P0-7 本地登录页无 Host/Origin 校验、无 CSRF

**`provider/src/xunlei/login_page.rs:78-103`**

```rust
pub fn login_router(session: Arc<LoginSession>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/login/pwd", post(pwd_login))   // ← 接收明文密码
        ...
        .with_state(session)                         // ← 无任何 .layer()
}
```

**已验证（量化）**：生产代码 **299 行**（L1-299，`#[cfg(test)]` 从 L300 开始），
以下模式**全部 0 次命中**：

```
from_fn: 0   middleware: 0   layer: 0   HeaderMap: 0
Origin: 0    HOST: 0         headers(): 0
```

即登录页**完全没有 HTTP 头访问能力**，不存在 Host/Origin 校验的可能。

**对比**：daemon 的 `http.rs:2622` 有完整 `auth_mw`（Host/Origin 守卫 + 常量时间 token）。
**同一代码库，一处做了防护，另一处没做。**

#### 攻击链（DNS rebinding）

1. 用户运行 `smart-dl-daemon xunlei-login --page` → 本地起 `127.0.0.1:<port>`
2. 用户浏览器访问 `http://evil.example`（攻击者域名，DNS 指向 `127.0.0.1`）
3. 页面 JS 执行 `fetch("http://evil.example:<port>/api/login/pwd", {method:"POST", ...})`
4. **浏览器视为同源** → **不受 CORS 限制**（浏览器只限制**读**响应，不限制**发送**简单/JSON POST）
5. 攻击者用**自己的账号**写入登录态，或诱导用户在该页面输入**自己的迅雷密码**

#### 可利用性限制（诚实说明）

| 条件 | 影响 |
|------|------|
| **端口随机**（`--port` 默认 0） | 攻击者需先探知端口 → **降低可利用性** |
| 用户显式 `--port` 或端口顺序可预测 | 攻击成立 |
| DNS rebinding 需受害者访问恶意页**且保持一段时间**（等 TTL 过期重解析） | 需要社工配合 |

**故定级 P0（安全）而非"随时可远程利用"** —— 但防护成本极低（一个 Host 校验中间件），
且明文密码接收端的存在使后果严重。

**修复**：加 Host 白名单（仅 `127.0.0.1:<port>` / `localhost:<port>`）+ 校验 `Origin`，
或生成一次性 token 注入 HTML 并强制校验。可直接复用 daemon 的 `request_from_local_origin`。

---

### 🔴 P0-8 夸克任务 ID 只含分享 ID → 句柄覆盖

**`provider/src/quark/share.rs:215`**

```rust
let pid = format!("{}-{}", self.name, link.pwd_id);  // pwd_id 对同一分享是常量
self.tasks.lock().await.insert(pid.clone(), QuarkTaskHandle { save_task_id, fids });
```

**影响**：同分享并发任务或 resubmit → 后写覆盖前者 → `resolve()` 拿到**别人的 fids 直链** → **文件内容错配写入磁盘（数据损坏）**。

**对照**：`MockProvider::next_id` 用了原子自增，**真实 provider 反而没用**。

---

### 🔴 P0-9 迅雷任务 ID 用秒级时间戳

**`provider/src/xunlei/provider.rs:249-253`**

```rust
let pid = if resp.task_id.is_empty() {
    format!("{}-{}", self.name, now_unix())    // ← 秒级，磁力提交必走此分支
} else {
    format!("{}-{}", self.name, resp.task_id)
};
```

同文件 L298 注释自认「磁力提交响应里 file=null、file_id 为空」→ **恒走秒级分支** → 同秒双提交互相顶掉。

---

### 🔴 P0-10 `PUT /settings` 可触发 `process::exit` / 系统关机

**`daemon/src/state/lifecycle.rs:82` + `settings.rs:590` + `ops.rs:40-78`**

```rust
// settings.rs:578-595  — 仅当显式提供 completion_action 字段时
if let Some(v) = &s.completion_action {
    self.set_completion_action(&norm);   // → lifecycle.rs:82 → maybe_run_completion_action()
```

`execute_completion_action`（`lifecycle.rs:641-674`）：
- `exit` → `sleep(250ms)` + `process::exit(0)`
- `shutdown/sleep/hibernate` → **真的执行系统命令**

**语义缺陷**：补判意图是「避免新设动作静默失效」，副作用是**设置即执行**——用户在任务全终态时设置 `"shutdown"`（正常用法），会**立刻关机**而非等下一个任务完成。

**修正说明**：原报告称「任意 `PUT /settings` 即可触发」**不准确** —— 仅当 body 含 `scheduler.completion_action` 时。且 `completion_fired` 的 CAS 会阻断重复触发。但**首次设置时确实会立刻执行**。

---

## 2. P1 汇总（按主题聚类）

### 2.0 `piece_length` 无上限 → 整数溢出 ★已实证

**`daemon/src/state/bt_alerts.rs:65`（赋值）+ `core/src/xltd.rs:151-152`（使用）**

```rust
// bt_alerts.rs:65  — 来自 .torrent 文件（外部输入），仅 `as u32` 窄化
piece_length = be_int(b, i)... as u32;

// bt_alerts.rs:128 — 全仓唯一校验，只挡 0
if piece_length == 0 || pieces_hash.is_empty() { ... }

// xltd.rs:151-152  — 无保护乘法
let piece_start = (idx as u64) * (piece_length as u64);
let piece_end = piece_start + piece_length as u64;
```

**已实证**（`repro/p0repro/src/plength.rs`）：

```
piece_length = 4294967295
idx          = 4294967298
真实乘积 (u128) = 18446744078004518910   ← 超过 u64::MAX
release 下回绕  = 4294967294            ← 完全错误的值
```

另外 `xltd.rs:131` 的 `file_size.div_ceil(4096) * 4096` 在大 `file_size` 下也溢出
（debug panic，release 回绕为 `0`）。

**影响**：release 下乘法回绕 → 区间判定 `[piece_start, piece_end)` 与真实位置无关
→ **读错位置 / 漏标完成 / 误判 partial（静默数据损坏）**。

**修复**：`TorrentMeta::parse` 加 `piece_length > 16MiB → InvalidSource`
（libtorrent 自身上限 `1<<24`）；计算点改 `checked_mul`/`checked_add`。

### 2.0a `is_loopback_addr` 前缀绕过 → 未鉴权绑公网 ★已实证

**`daemon/src/config.rs:443`**

```rust
host.eq_ignore_ascii_case("localhost") || host.starts_with("127.") || host == "::1"
```

**关键**：`starts_with("127.")` 是**字符串前缀**，不校验后续是否为合法 IPv4。
`127.evil.com:8787` 在 L435/L440 两次 `parse` 都失败 → 落到 L443 前缀分支 → 放行。

**已实证**（`repro/p0repro/src/round3.rs`）：

```
addr                              原实现   加固版
127.0.0.1:8787                    true     true    正确
127.evil.com:8787                 true     false   ★ 绕过：DNS 可解析到公网
127.0.0.1.attacker.example:8787   true     false   ★ 绕过
127.0.0.1.nip.io:8787             true     false   ★ 绕过：nip.io 万能解析
127.0.0.1x:8787                   true     false   ★ 绕过
0.0.0.0:8787                      false    false   正确拒绝
原实现错误 4 项
```

**下游**（`serve.rs:47` 的 fail-closed 闸门）：

```rust
if !Config::is_loopback_addr(&cfg.server.addr) && http_token.is_none() {
    return Err(ServeError::Engine("检测到非回环监听地址但未配置 http_token…"));
}
```

`addr = "127.evil.com:8787"` + 未配 token → **闸门放行** → `TcpListener::bind` 经 DNS
解析到公网 IP → **API（含写文件 / hook 执行链）绑公网且无鉴权**。

**测试盲区**：现有测试覆盖了 `0.0.0.0` / `192.168.x`，**恰好没覆盖 `127.` 前缀的非 IP 主机名**。

**修复**：删掉前缀兜底，只保留 `localhost` / `::1` 字面量；非 IP 主机名一律 fail closed。

---

### 2.0b 容器级 `#[serde(default)]` → 部分表默认值静默错位 ★已实证

**`daemon/src/config.rs:32-34 / 49-51 / 78-80`**

`Config` 的子表用 `#[derive(Default)]` + 容器级 `#[serde(default)]`，
但 `Config::default()` 里手写了非类型默认值（`dest_root: "./downloads"`、`bt.enabled: true`）。

**后果**：**只写同表的一个键，其它键会退化为 `derive Default` 的类型零值。**

**已实证**（`repro/p0repro/src/round3.rs`）：

```
[整表缺失]     dest_root = "./downloads"     ← 走 Config::default()，正确
[只写 proxy]   dest_root = ""                ← 退化为 derive Default
```

| 用户操作 | 静默后果 |
|----------|----------|
| `[download] proxy = "socks5://..."` | `dest_root` 变**空串** → 落盘位置变成"相对 cwd" |
| `[server] http_token = "x"` | `addr` 变 `""` → `bind("")` 失败 → **daemon 起不来** |
| `[bt] max_upload_kb_s = 100` | `bt.enabled` 变 **false** → **BT 静默关闭** |

**注意**：这与"缺 `#[serde(default)]` 导致解析失败"是**反向**关系 ——
容器级 default 保证了不解析失败，代价是引入默认值错位。

**测试为何没抓到**：`config.rs:634` 的 `assert!(c.bt.enabled)` 之所以通过，
是因为它的配置**整表缺失**才走 `Config::default()`。

**修复**：为各子表手写 `impl Default`，值与 `Config::default()` 一致
（`BtCfg` 已为 `encrypt` 这么做了，扩展成整体即可）。

---

### 2.0c `sanitize_rel` 放行 `CurDir` ★已实证

**`core/src/session/output.rs:22`**

```rust
for comp in pb.components() {
    match comp {
        std::path::Component::Normal(_) | std::path::Component::CurDir => {}  // ← CurDir 放行
        _ => return Err(OutputError::UnsafePath(rel.into())),
    }
}
// L30  仅在 file_name() 为 Some 时校验保留名
if let Some(name) = pb.file_name().and_then(|n| n.to_str()) { ... }
```

**已实证**：

```
sanitize_rel(".")    -> Ok  dest = "D:/dl/."      ← 指向 root 自身
sanitize_rel("./")   -> Ok
sanitize_rel("a/b/") -> Ok                        ← 尾随分隔符
Path::new(".").file_name() = None                 ← 整块保留名校验被跳过
```

**因果链**：L22 放行 `CurDir` → `"."` 通过 → L30 `file_name()` 返回 `None`
→ **L30-43 的整个保留名/尾点校验块被跳过**。

**后果**：`dest == dest_root`（目录）；`finalize` 时 `dest.exists()` 为真、
`metadata().len()` 为目录长度 → 若恰等于 `expected_size` 则走幂等短路
→ `remove_file(目录)` 失败但被 `let _ =` 吞掉 → **静默「成功」但文件未落位**。

**对照组**：真穿越仍被正确拒绝（实测 `../escape`、`/abs`、`C:/x`、`a/../../b` 全部 Err）。

**附带（P1-2）**：保留名 stem 未 `trim_end_matches([' ', '.'])` →
实测 `"con .txt"` / `"COM1 .txt"` **放行**（Windows 上等价于设备名）。

---

### 2.1 持锁做阻塞操作（系统性模式）

| 位置 | 问题 |
|------|------|
| `httpdl/rate.rs:103-108` | 限速器**持锁 sleep** → 跨段串行，速率低于设定值；取消时债务不回滚 |
| `daemon/persistence.rs:130-142` | `autosave` 同持 `persist_lock` + `tasks` 锁，**持 `tasks` 锁做同步磁盘 I/O** |
| `daemon/ops.rs:57-72` | completion 判定**持 tasks 锁**，而 exit 分支 sleep 250ms |
| `daemon/rss.rs:664-687` | RSS 添加**持锁做整表 JSON 落盘** |
| `bt.rs:236-243` | （已列 P0-2）guard 跨 FFI |

**共通根因**：**磁盘 I/O / FFI / sleep 被放进了临界区**。

### 2.2 目录与凭据权限（跨文件同步缺失）

| 位置 | 问题 |
|------|------|
| `provider/quark/types.rs:61` | `std::fs::write` **先以 0644 创建**再 chmod（L52 注释自认"此前漏同步"） |
| `daemon/nas.rs:497-510` | `put_auth_token` **无权限收紧、无 fsync** → OAuth token 0644 |
| `daemon/nas.rs:302` | `engine.log` 无轮转、无上限、0644 |

**对照正确实现**：`provider/xunlei/auth.rs:163-176` 用 `OpenOptionsExt.mode(0o600)` 先建后写，注释明说「消除 0644 窗口」。

### 2.3 持久化可靠性

| 位置 | 问题 |
|------|------|
| `daemon/persistence.rs:12-54 / 69-97` | 原子写**全程无 fsync** → 掉电后 `tasks.json` 0 字节 → **全部任务丢失** |
| `daemon/config.rs:397-416` vs `serve.rs:461` | 热重载走裸 `toml::from_str`，**绕过全部语义校验** |

**fsync 对照**：`Config::save_to`（`config.rs:467`）**做了** `sync_all`，而 `tasks.json` 更重要却漏了。

**校验绕过实例**：`bt.max_share_ratio = -1.0` 启动时被拒，热重载却接受 → 负阈值使"分享率达标记"恒真 → **任务一进 Seeding 立刻暂停**。

### 2.4 单实例锁竞态

**`daemon/lockfile.rs:68-73`**

```rust
impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();          // ① 解锁
        let _ = fs::remove_file(&self.path); // ② 再删文件 ← 窗口
    }
}
```

**经典 unlink 竞态**：A 解锁后、删文件前，B 打开同一 inode 加锁成功；A 删掉路径；C 启动时 `create(true)` **新建 inode** 并加锁成功 → **B 与 C 同时运行**。

**修复**：Drop 里**不要删锁文件**（只 unlock）。锁文件残留无害（判据是 `try_lock_exclusive`）。

### 2.5 ID 生成不一致

| 实现 | 方式 | 风险 |
|------|------|------|
| `MockProvider::next_id` | 原子自增 ✅ | — |
| `quark/share.rs:215` | `pwd_id`（常量） ❌ | 覆盖（P0-8） |
| `xunlei/provider.rs:250` | 秒级时间戳 ❌ | 覆盖（P0-9） |

### 2.6 其余 P1

| 位置 | 问题 |
|------|------|
| `httpdl/rate.rs:130-148` | `RateSample` 短窗口丢字节 → 速率低估 |
| `httpdl/rate.rs:72-74` | 限速热调不清 `next` → 旧债务继续生效 |
| `httpdl/ftp.rs:701-704` | PASV 六元组无范围校验 → **SSRF 防护失效** |
| `httpdl/ftp.rs:1334-1336` | `is_terminal` 的 `starts_with("5")` 包含 550 → 停用二分重试 |
| `httpdl/sftp.rs:173-176` | 靠英文文案判永久错误 |
| `httpdl/hls.rs:583`、`dash.rs:1070` | 固定 `.tmp` 名（`ledger.rs` 已修，这两处漏改） |
| `daemon/rss.rs:895-909` | 未处理条目**无上限**，单调增长 |
| `daemon/bt.rs:284-287 / 566-583` | `alert_loop_active` 单向置位；`pause` 失败不回滚意图 |
| `daemon/bt.rs:578-612` | `let _ =` 吞掉 fastresume 写盘失败与 panic |
| `daemon/nas.rs:341-350` | `stop` 不 reap → **僵尸进程**；只按 pid 杀 → **复用误杀** |
| `provider/xunlei/provider.rs:203-221` | `refresh_auth` 持锁跨 await（最长 30s）→ 全 provider 串行 |
| `provider/coordinator.rs:170-204` | 恢复流**只修记账不修行为** → 重传全部文件（最坏 4× 流量） |

---

## 3. P2 精选

| 位置 | 问题 |
|------|------|
| `daemon/http.rs:2584-2588` | **`DELETE /security/bans` 带 body** → HTTP 语义违规，AI/多数客户端不支持 |
| `daemon/http.rs:2560-2615` | API 动词不一致：26 个 POST，改名/打标签/改限速都该用 PATCH |
| `daemon/http.rs:2688` | tokenless 模式 Host 缺失时 `None => true` 放行 |
| `daemon/http.rs:2651-2661` | token 经 query 传递 → 进日志/历史/Referer |
| `daemon/state/ops.rs` | **4534 行单 impl 块**，69 方法；4 个 `add_*_task_opts` 高度重复 |
| `daemon/state.rs:577-696` | `DaemonState` **30+ 把独立锁**，锁序靠人工保证 |
| `daemon/bt_events.rs:57`、`serve.rs:411` | loop 全部「先 sleep 后工作」→ 启动盲区 |
| `daemon/serve.rs:409-513` | 长驻循环**无取消路径** |
| `httpdl/engine.rs:840` | 日志 typo `"ttpdl]"` 应为 `"[httpdl]"` |
| `daemon/nas.rs:104-184` | tar **symlink 成员可先落盘**，`verify_no_escape` 是事后报警 |
| `daemon/lifecycle.rs:553-563` | dest 白名单**只增不减** |
| `provider/coordinator.rs:252-267` | `last_verdict` 是死代码 |
| `provider/xunlei/provider.rs:433` | 恒真条件重复比较 |
| `daemon/rss.rs:133-134` | `.corrupt` 固定名覆盖 |

---

## 4. 明确「未发现」的类别

**这部分与发现问题同等重要** —— 避免重复排查。

### 4.1 并发与锁

| 类别 | 结论 |
|------|------|
| `parking_lot` guard 跨 `.await` | **未发现**（除 `bt.rs:236` 那个 P0）。ops.rs 三处均用块表达式收在同步段内 |
| 锁序反转死锁 | **未发现**。方向恒为 `persist_lock→tasks`、`tasks→slot_reservations`，无环 |
| 锁内重入同锁 | **未发现**。历史 Bug B 已修 |
| `select!` 取消安全 | **未发现**（全域仅 `http.rs:1529` 一处） |
| 无界 channel | **未发现** |
| `std::sync::Mutex` 混用 | **未发现** |

### 4.2 FFI（184 处 unsafe 手工核）

| 类别 | 结论 |
|------|------|
| `CString` 悬垂 | **未发现**。全部绑定 `let` 变量存活到调用点（`identity.rs:116` 注释明确防此坑） |
| 可选符号未检查 | **未发现**。4 个可选符号调用侧全用 `ok_or_else` |
| `lt_session_new` 泄漏 | **未发现**。C++ 侧 `*out` 在 `new` 成功后才赋值 |
| `Session` 双释放 | **未发现**（无 `Clone`）。但 `unsafe impl Send/Sync` 缺安全论证，**脆弱** |
| libloading 符号解析 | **未发现**。全部 `map_err`，可选符号 `.ok()` 降级 |

### 4.3 其余

| 类别 | 结论 |
|------|------|
| 段边界 off-by-one | **未发现**（三处口径一致） |
| 并发写同一文件 | **未发现**（gen/epoch 双闸门有效） |
| >4GB u64 溢出 | **未发现**（逐处核对全部 `as` 窄化）**例外**：`piece_length` 无上限（见下） |
| FTP 命令注入 | **未发现**（CR/LF/NUL 三层拦截） |
| 文件句柄泄漏 | **未发现** |
| XXE（metalink） | **未发现**。quick-xml 不支持 DTD 外部实体 |
| `ws.rs` 队列无界 | **未发现**（严格 `cap` 守卫） |
| `events.rs` 标签一致性 | **未发现**（match 无通配臂，新增变体编译失败） |
| `health.rs` 除零 | **未发现**（显式处理 `total_download == 0`） |
| `cli.rs` 参数解析 | **未发现** |
| `mock.rs` 进生产 | **未发现** |
| provider 凭据进日志 | **未发现**（整个 crate **零日志调用**） |
| provider 硬编码盐 | **已确认存在**，但属逆向必需（协议方不提供替代） |
| `nas` 路由鉴权 | **未发现缺陷**（`auth_mw` 是 Router 全局 layer） |
| metalink 大文件 | **未发现**（`ops.rs:1870` 有 16MB 上限 + 流式累计校验） |

---

## 5. 两个系统性模式（比单个缺陷更重要）

### 5.1 「在一处修了，没同步到结构同构的另一处」

| 修复位置 | 漏改位置 | 性质 |
|----------|----------|------|
| `xunlei/auth.rs:163` 先 0600 建文件 | `quark/types.rs:61` 先写后 chmod | 凭据可读窗口 |
| `ledger.rs:119` 纳秒唯一 tmp 名 | `hls.rs:583`、`dash.rs:1070` 固定 `.tmp` | 并发互踩 |
| `http.rs:2622` Host/Origin 守卫 | `login_page.rs:78` 零中间件 | **DNS rebinding** |
| `MockProvider::next_id` 原子自增 | `quark/share.rs:215`、`xunlei/provider.rs:250` | 句柄覆盖 |
| `Config::save_to` 有 `sync_all` | `persistence.rs:12` 无 fsync | 掉电丢数据 |
| `dash.rs:1017` 改流式 | `hls.rs:484` 仍 `resp.bytes()` | 内存峰值 |
| `config.rs:397` 有校验 | `serve.rs:461` 热重载裸解析 | 校验绕过 |
| `bt_alerts.rs:128` 挡 `piece_length == 0` | `core/torrent_meta.rs:238` 不校验 | **口径不一致** |
| `resolve` 用局部 `file_size` | `xltd.rs:151` 未用 checked 算术 | 整数溢出 |

**建议**：修复某模式时**全仓 grep 同构代码**，而非只改当前文件。

### 5.1b 测试固件与真实数据形态不一致

| 位置 | 问题 |
|------|------|
| `torrent_tests.rs:90-112` | fixture 用**非法 bencode 键序**（`piece length` 在 `files` 前），真实种子相反 → 1180 测试全绿但线上必崩 |
| `config.rs:634` | `assert!(c.bt.enabled)` 仅在**整表缺失**时成立，未覆盖"部分表"情形 |
| `config.rs:597-598` | loopback 测试覆盖 `0.0.0.0`/`192.168.x`，**恰好漏掉 `127.` 前缀非 IP 主机名** |

**建议**：涉及外部格式（bencode / TOML）的测试，fixture 应**从真实样本生成**而非手工拼接。

### 5.2 「注释与实现脱节」

| 位置 | 注释声称 | 实际 |
|------|----------|------|
| `share.rs:439` | "与 captcha/init 一致" | 设备号**每次不同** |
| `ops.rs:36-39` | "锁内停留亚毫秒" | exit 分支 **sleep 250ms** |
| `lifecycle.rs:319-324` | "任何时刻至多持一把锁" | `autosave` **同持两把** |
| `quark/types.rs:52` | "quark 侧此前漏同步" | 只改注释，**未改实现** |
| `coordinator.rs:187` | "已修重复传输" | 只修**记账**，未修**行为** |

**建议**：审计修复时同步更新受影响的历史注释；注释中的"已修复"应可被测试验证。

---

## 6. 修复优先级

| 顺序 | 项 | 理由 |
|------|-----|------|
| 1 | **P0-1 除零 panic** | 导入真实种子即崩；修复量小；**有复现** |
| 2 | **P0-11 total_size 溢出** | 绕过磁盘预检 → 写爆磁盘；**有复现**；改 `checked_add` |
| 3 | **P0-2 guard 跨 FFI** | 改两行；影响全任务 fastresume |
| 4 | **P0-3 poll_ring 互吞** | 任务永久卡住；含根因一次闭合 |
| 5 | **P0-7 登录页无防护** | 安全；可窃取明文密码 |
| 6 | **P1-a loopback 绕过** | 未鉴权绑公网；**有复现**；删前缀兜底即可 |
| 7 | **P0-6 设备号不匹配** | 匿名分享 100% 失败 |
| 8 | **P0-8/P0-9 ID 覆盖** | 数据损坏；改 ID 生成即可 |
| 9 | **P0-5 无 ETag 混拼** | 静默损坏，主流场景 |
| 10 | **P0-4 空洞文件** | 需先定策略 |
| 11 | **P0-12 core piece_len** | 与 daemon 口径统一 |
| 12 | **P1-b 部分表默认值** | 静默关闭 BT / 起不来；**有复现** |
| 13 | **P0-10 设置即执行** | 语义缺陷，需确认设计意图 |
| 14 | **P1-1 sanitize_rel CurDir** | 安全关键函数；**有复现** |
| 15 | **P1-2 保留名 trim** | 一行修复 |
| 16 | **P1 锁内 I/O 族** | 系统性，建议一次重构 |
| 17 | **P1-2 权限同步族** | 按 §5.1 模式全仓扫 |

---

## 7. 复现工程

**`repro/p0repro/`** — 独立 crate（`[workspace]` 已隔离，不参与主 workspace 构建）

```
repro/
├── P0-EVIDENCE.md              第一轮 P0 证据
└── p0repro/
    ├── Cargo.toml
    └── src/
        ├── lib.rs              提取自 bt_alerts.rs L5-161/163-254/449-538
        ├── plength.rs          piece_length 溢出
        ├── round2.rs           total_size 溢出 + sanitize_rel
        └── round3.rs           loopback 绕过 + 部分表默认值
```

**运行**：

```powershell
cd repro\p0repro
cargo test -- --nocapture
```

**当前结果**：`10 passed / 3 failed`
—— **3 个 failure 就是 3 个真实缺陷的证明**（除零 panic、total_size 溢出 ×2），
其余 10 个 pass 包含对照组与加固版验证。

---

## 8. 待补

| 区域 | 行数 | 状态 |
|------|------|------|
| `btcore` 非 FFI 业务层 | ~1,900 | 未审（FFI 层已手工核） |
| `xunlei-ffi` 非 FFI 部分 | ~1,700 | 未审 |
| `xunlei-convert` 剩余 | ~1,000 | `fastresume.rs` / `xltd.rs` 已核，其余未审 |
| `daemon` 测试代码 | — | 未审（生产代码已全覆盖） |

**已覆盖**：httpdl 100% / daemon ~100% / provider 100% / core 100% = **约 45,800 行**
