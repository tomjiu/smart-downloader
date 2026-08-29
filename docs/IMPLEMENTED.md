# 已完成事项记录（IMPLEMENTED）

> 用途：**已落地功能**的集中档案（与 `BACKLOG.md` 的"未实现清单"互补）。
> 每条含 commit 与验证证据；"行为契约"为实操查阅要点（配置 / API / 语义）。
> 只记录事实与边界，不讨论设计。

## 通用能力（2026-08-19 批次）

### 1. 全局代理 + 下载/上传限速（`3fac8e3`，补充测试 `12b0101`）

**配置**（启动时生效，**不参与热重载**——避免重建连接）：

```toml
[download]
proxy = "http://host:port"        # 或 socks5:// / socks4://
max_download_kb_s = 2048          # 全局下载限速（HTTP + BT 共用）；0 = 不限
[bt]
max_upload_kb_s = 512             # BT 上传限速；0 = 不限
```

**行为契约**
- HTTP 引擎经 reqwest `Proxy::all` + 凭据（URL 内 `user:pass@` → basic_auth）；BT 引擎经内核 `lt_apply_network`（settings_pack，对齐 libtorrent 2.x：字段名 `proxy_hostname`）。
- 限速：跨段共享 `RateLimiter`（deadline 链）。**冷启动语义**：无积压时首个 chunk 立即放行（突发恢复不欠债），连续消费才节流。
- 敏感项：**proxy URL 不出现在 `/config` 快照**（仅暴露 `proxy_enabled`）。

**验证**：config 快照断言 5 用例、parse_proxy 5 用例、RateLimiter 时序 4 用例、引擎级限速集成 2 用例（限速生效对照 + 完整性 SHA256 一致）。

**已知边界**：代理实际转发需真实代理实连确认（本地仅验证解析/构建/启动路径）。

### 2. M6：云兜底调度接线（`c19313a`）

**API**：`POST /tasks/:id/fallback`（原 501 桩 → 真实执行）

**前置条件（FallbackPolicy 默认冻结）**
1. 任务必须是 **BT 任务**（HTTP/FTP 任务 → 409 "仅 BT 任务支持云兜底"）
2. 任务必须**已暂停**（串行策略：禁 BT/直链双份占盘 → 409 "需先暂停"）
3. BT 进度 **< 50%**（≥50% → 409 "仅进度 <50% 可兜底"）
4. 存在**可用 provider**（无 → 409 "无可用 provider"；不可用含未配置/未认证/配额耗尽/冷却/并发满）

**成功语义**：选 provider → submit → Ready → resolve 直链 → HttpEngine 传输（每个直链一个引擎任务，轮询终态，60s 超时）→ BT 引擎任务退役（keep data）→ 任务置 `Completed` + `StateChanged`/`Completed` 事件广播 + 落盘。响应 200：`{"status":"completed","provider","provider_task","transferred":[...]}`。
直链过期恢复：update_sources ≤3 → resubmit ≤2 → 超限 Failed（409）。

**配置**

```toml
[provider]
enabled = true   # 兜底总开关（默认 false：不自动烧配额）
mock = true      # 开发/演示 MockProvider（唯一现成实现；真实 provider 待迅雷线落地）
```

`GET /providers` 列出 provider 运行态（enabled/authenticated/quota/backoff/busy）。

**错误映射**：404（任务不存在）/ 409（上述语义错误）/ 500（其它引擎错误）。

**验证**：`fallback_api` e2e 2 用例（直链成功落盘 + SHA256 内容一致；未暂停 409；无 provider 409）、http_api 404/409 语义 2 用例、serve 冒烟（`[provider] mock=true` → `/config.provider_enabled=true` + `/providers` 列出 mock）。

**顺带修复（同 commit）**
- BT/magnet/.torrent 任务 dest 缺省值 = 配置 `dest_root`（原为 `.`，与 HTTP 不一致）
- pause/resume 后任务记录缓存同步（原 alert 流不迁移 pause，list/快照与 API 动作不一致）

**已知边界**：provider 目前只有 MockProvider（占位）；真实 provider（迅雷云盘等）属迅雷线，落地后按 `RemoteProvider` trait 注入即可，调度骨架已就绪。

### 3. http_api 轮询护栏修复（`051c8a4`）

**根因**：重负载窗口（连续重建 + 杀软扫描新编译 exe）的进程级停顿击穿轮询用例 10s 等待护栏 → 偶发单用例超时失败（曾观测 3 次，均"16 passed; 1 failed"；50+ 次运行 + 6 轮强制 rebuild 首跑全绿，与负载停顿吻合）。

**修复**：`crates/daemon/tests/http_api.rs` 三处等待护栏（快照 snapshot / list / 事件 event）**10s → 60s**。语义不变（最终一致性断言、100ms 轮询），对进程级停顿免疫。

**验证**：修复后 http_api 17/17、fmt 0、clippy 0。

## 能力四卡 + 迅雷登录终局（2026-08-25 批次）

> 本批次代码在工作区（截至记录时未提交）；条目沿用既有格式（行为契约/配置/API/验证证据）。
> 已知问题（Bug A/B/C）见文末第 6 条，明细引用 [`NEXT_ACTION.md`](research/xunlei/NEXT_ACTION.md) 尾部表。

### 1. FTP 目录下载（引擎 LIST 展开 + daemon 路由 + 快照 files 透出）

**行为契约**
- `POST /tasks` 接受 `ftp://host/path/` 目录 URL → daemon `add_ftp_task` 路由打通 → httpdl 引擎对目录做 LIST 递归展开 → 目录任务按多文件逐个下发；根目录 URL 落盘到主机名目录，子目录落到末级目录名下。
- 空目录 add 直接失败（不产生空任务）。

**API / 配置**：无新增配置项；入口即既有 `POST /tasks`（`ftp://` 目录 URL 可达）。

**快照透出**：`TaskSnapshot.files: Vec<FileProgress>` 透出每个子文件的路径/大小/进度——多文件任务不再"只见总量不见明细"。

**验证**：`crates/httpdl/tests/ftp_directory.rs` 3 例（目录落在目录名下 / 根目录落在主机名下 / 空目录 add 失败）；daemon 侧 mock FTP e2e（`state.rs`）覆盖路由到落盘全链。

### 2. BT 发现层开关（DHT/LSD/UPnP）

**配置**（默认全关，保持 M0 确定性）：

```toml
[bt]
enable_dht = false   # magnet 无 tracker 冷启的关键开关
enable_lsd = false
enable_upnp = false
```

**行为契约**：config → BtCore 会话构造 → FFI `lt_apply_discovery(enable_dht, enable_lsd, enable_upnp)` 全链路接线，libtorrent settings_pack 生效；`/config` 快照新增三键 `bt_enable_dht` / `bt_enable_lsd` / `bt_enable_upnp`（热重载可见）。

**验证**：config 解析+快照断言（默认 false / TOML 开启 / 热重载翻转）；真实网络人工验证待跑——G1 手动脚本 `scripts/manual/G1_dht_coldstart.ps1`（DHT-on 找到 `num_peers > 0`，全关对照保持 0 peer）。

### 3. .torrent 多文件空间预检

**行为契约**：BT 任务 add 时解析 .torrent 的 `info/files` 各项长度求和（多文件）或单文件 `length`（回退路径），得总大小后走统一 `precheck_space(dest_root, total)` 磁盘空间预检；两路都拿不到大小才跳过预检（不阻塞添加）。

**实现**：`torrent_precheck_total(bytes) -> Option<u64>`（`crates/daemon/src/state.rs`），接入点在 infohash 定位之后、查重之前。

**顺带修复（同批次）**：xunlei-import 测试单独门控——`#[cfg(all(test, feature = "xunlei-import"))] mod xunlei_import_tests`，修复 `--features bt` 下编译失败（此前与 BT 测试混编互相拖累）。

**验证**：`torrent_precheck_total` 单测 3 例（30 文件求和 / 单文件 length / 缺 files 回退最小解析）。

### 4. P2SP webseeds 注入端点（F5.1 Rust 实装落地）

**API**：`POST /tasks/:id/webseeds`，请求体 `{"urls": ["http://...", ...]}` → 返回实际注入条数；仅 BT 任务可用（409/404 语义同既有任务端点）。

**行为契约**：daemon `add_webseeds` → `BtEngine::add_url_seed` → FFI `lt_add_url_seed`（libtorrent BEP-19 web seed）。用途：给 BT 任务注入迅雷云盘直链等 HTTP 源做 P2SP 混合加速；直链时效 ≈1h 可重复调用换链（直链 query 含防篡改签名 `at=`，禁止改动参数）。

**验证**：e2e 本地 HTTP 源测试（起本地 HTTP 服务作 web seed，断言注入生效 + 任务可达完成态）；btcore 层 `status_controls.rs` 直调用例。手动项 G2：`scripts/manual/G2_proxy_live.ps1`（代理 live 转发人工验证脚本，本批次随卡补齐）。

### 5. 迅雷登录终局（网页 localStorage 凭据 + 自动续期）

**鉴权配方**（配方脚本 [`web_token_validate.ps1`](../scripts/research/xunlei/web_token_validate.ps1)，Rust 侧 `auth.rs` 同构实现）：
- 票源：浏览器 pan.xunlei.com 页面 localStorage `credentials_Xqp0kJBXWhwaTpB6`（aud=Xqp0 为 api-pan 白名单正主）；凭证文件 `xunlei_auth_web.json`（**已 gitignore**，`.gitignore` 补 `_web.json` 防泄漏）。
- captcha/init meta **全配方**：`client_version + package_name + user_id + captcha_sign + timestamp` 五件套 + Bearer 头（此前全部失败的根因就是缺 user_id/captcha_sign 等）。

**自动续期**：refresh_token（a1. 格式）12h 续期实测通过 → 一次开户长期免维护；provider `submit/status/resolve` 三入口前置 `refresh_auth()`（access+captcha 自动续期并回写旋转凭据）；`auth.rs` 新增 `jwt_exp` / `from_web_credentials_str` / 随机 did32。

**验证**：example `xunlei_live_check.rs` 活体自检一次通过（load→refresh→captcha→list→PLAY→Range206 全链）；provider 测试 77 绿、clippy 归零。

### 6. 已知问题：三只 Bug A/B/C（引用表，不在此重复展开）

明细见 [`NEXT_ACTION.md`](research/xunlei/NEXT_ACTION.md) 尾部「F3.1 验收发现的三只真 Bug」表：

| # | 一句话 | 状态 |
|---|---|---|
| A | 磁力任务 pause 后引擎队列复活继续下载 | **已修（调度层持续执法）**：`enforce_pauses` 每 500ms 对比 done 增长再压；彻底根治仍需 metadata alert 尊重记录态 |
| B | 特定生命周期交汇窗口 runtime 全端点挂死（29 线程全 Parked 死锁实锤） | 待专项（干净环境单变量复现→minidump→线程栈定位持锁者） |
| C | 兜底传输撞上已抢先下完的同名文件时挂起 | 待专项（疑点 FallbackSink→HttpEngine.add 已存在终文件分支；候选短路方案已列） |

### 7. Provider 自动降级（探活 + 失败冷却）

**行为契约**
- `RemoteProvider::probe()` 轻量探活（默认 `Ok(()))`；`XunleiProvider` 重载为检查登录态 + access_token 未过期）。
- 单 provider 失败（submit/status/resolve/handle_links）自动记录冷却：Auth 5 分钟 / Quota 1 小时 / 其他 1 分钟。
- `FallbackCoordinator::begin_fallback` 改为多 provider 依次尝试循环：单个 provider 失败不阻塞主链路，自动切换下一个可用 provider。
- `XunleiProvider::runtime()` 新增 `backoff_until` 字段，`GET /providers` 可查冷却倒计时。

**验证**：provider 单元测试 85 绿（含 `probe_ok_when_auth_loaded` / `probe_err_when_token_expired` / `submit_sets_backoff_on_error` / `runtime_reflects_backoff_countdown`）；daemon `fallback_api` 5 例绿（新增 disabled/quota/auth 降级 e2e）。

### 8. http_api magnet 创建 BT 任务测试修复

**根因**：`crates/daemon/tests/http_api.rs` 的 `serve()` 在 `--features bt` 下未起 `BtEngine`，导致 magnet 链接返回「引擎未加载: Bt」。同时测试用了非法 infohash `abc`（非 40 hex），即使起了 BT 引擎也会报 `LT_ERR_ENGINE`。

**修复**
- `serve()` Conditional 起 `BtEngine`（与 `fallback_api.rs` 同模式），dest 指向独立 tempdir。
- 断言改为合法 magnet（40 hex infohash）→ 201 + `task_id` 以 `t` 开头。
- 创建后立刻 remove，避免 save_path 污染后续测试。

**验证**：`cargo test -p smart-dl-daemon --features bt --test http_api` 17 passed。

### 9. xunlei-import 端到端集成测试

**背景**：`POST /tasks/xunlei-import` 路由代码已存在（`http.rs`），但缺少 HTTP 层 e2e 测试；现有 `state.rs` 内单测直接调 `DaemonState::add_xunlei_import_task`，未经过 axum router / Json extractor / 400 映射。

**新增测试**：`crates/daemon/tests/xunlei_import_api.rs`（feature `xunlei-import`）
- `xunlei_import_creates_bt_task`：用 `tools/xunlei-migrate/e2e_out` 真实样本（test.torrent + test.xlbt.cfg + test.bt.xltd）POST 到 `/tasks/xunlei-import` → 201 + `task_id` + `engine=bt`。
- `xunlei_import_rejects_bad_base64`：非法 base64 字段 → 400 + `"base64"` 错误提示。
- `xunlei_import_rejects_xltd_count_mismatch`：2 文件 torrent 只传 1 个 xltd → 400 + `"不匹配"`。

**注意**：axum `Json` extractor 默认 1MB 限制，测试里用 `DefaultBodyLimit::max(10MB)` 放行 2MB xltd 样本。生产环境若遇到 413 需同样调大或前端分片。

**验证**：`cargo test -p smart-dl-daemon --features "bt,xunlei-import" --test xunlei_import_api` 3 passed。

### 10. 迅雷 P2P 研究文档收口（PHub 加密 + XUDT 密钥）

**背景**：早期逆向（v2）将 PHub HTTP body 加密误判为 `AES-ECB(MD5(cmd+seq), body)`；后续 v3 反汇编与真实样本证实生产路径为 RSA-wrapped random AES key。XUDT 帧密钥派生（`MD5(8_byte_header)`）此前散落于脚本目录，未并入主文档。

**收口动作**
- `docs/research/xunlei/p2p_recon_complete.md`：顶部加「勘误（2026-08-27）」条；所有 v2 MD5 公式段落增「仅 XUDT/legacy 适用」警告。
- `docs/research/xunlei/p2p_research_complete.md`：同勘误条 + 内联警告。
- `docs/research/xunlei/p2p_recon/PROGRESS_REPORT_v3.md` / `RESEARCH_STATE.md`：同上。
- `docs/research/xunlei/xunlei_engine_research.md`：6.3 节新增「XUDT 加密密钥（2026-08-22 确认，A 级）」段落；5 节主机清单后加 PHub 加密说明框。

### 11. httpdl 动态分段（P0，方案A：动态领取 + 流式写盘）`109692c`

**行为契约**
- 多连接不再静态等分：`SegmentManager`（`crates/httpdl/src/segment_manager.rs`）按 FIFO 动态领取段，粒度 `min_split`（默认 16MB，`DEFAULT_MIN_SPLIT`）；worker 池大小 = `clamp(total/64MB, 2, 8)`，完成一段立即领取下一段 → 慢段不拖尾、快 worker 不吃亏。
- 段内流式写盘（`download_dynamic`）：按 Range 分块读取直接顺序写入 `.part`（原整段读内存 `Vec<u8>` 后写盘已移除）→ 内存峰值 O(块) 而非 O(段)。
- 续传语义：`HttpTask.segments: Vec<Segment>` → `offset: u64`；续传时跳过 `[0, offset)` 视为已下载，动态领取剩余段；`.part.etag` 决策（`resume.rs`）不变。
- 失败语义：任一 worker 段全源失败 → 整体 Err（abort 其余 worker，不做部分成功利用）；P0 不做失败段回收（release 接口 P1 预留）。

**验证**：`segment_manager` 单测 7 例（全文件覆盖 / 尾段不足粒度 / 续传偏移计 done / 字节累计 / offset 边界 / 零长文件 / 默认粒度回退）；`cargo test -p smart-dl-httpdl` 全绿——含 64MB 4 段 SHA256 与源一致、段起点覆盖不重叠、mirror 接管、全源失败→Error、http_resume/resume_etag 续传等集成用例。

**边界**：<16MB 小文件只有 1 段 → 单 worker 下载；`plan_segments`（static_split）仍保留给 FTP 串行路径；`ftp.rs` 未接入动态分段（属另一条线）。

### 12. 失败缩小粒度重试（P1，`b70923e`）

**行为契约**：段全 mirror 失败不再直接整体 Err——`download_segment_with_retry` 按迭代式拆分栈拆半重试（粒度下限 1MB，`MIN_RETRY_GRANULARITY`），左右子段都成功才视为段成功；缩到最小粒度仍失败才报段错误并走整体 Err。子段写入各自区间，与整段写入等价；已成功子段字节不回收（重试覆盖写，语义无害）。

**验证**：新用例 `failed_large_segment_recovers_by_halving`——测试服务器 `fail_ranges_min_len` 模拟"起点 16MB 且长度 ≥ 8MB 的 Range 404"（大段失败、拆半后可下载），32MB 文件拆半收敛完成，文件 SHA256 与源一致；Range 起点留痕（16MB/20MB/24MB）验证拆分过程真实发生。既有 `all_mirrors_dead_reports_error` 语义不变：坏区无法通过拆分修复时仍整体 Error。

### 13. backup_url/backup_md5 备用源兜底（P1，`963f9dd`）

**背景**：夸克架构（`quark_architecture.md` / `cross_client_comparison.md`）的备用源切换机制——主源失败后切备用源，并以备用源内容校验值确认。

**行为契约**
- `DownloadSource::Http` 新增 `backup_url: Option<String>`；`ContentIdentity::SingleFile` 新增 `backup_md5: Option<String>`（均 `serde(default)`，旧数据兼容）。
- 校验优先级：有 `sha256` 用 SHA256；无 sha256 但有 backup_md5 用 MD5（`verify_file_md5`，`md-5 = "0.10"`）；均无 → 跳过校验直接落位。
- 主源两次校验失败（原降级阈值）后：若配置了 `backup_url` → 清空 sha256、置 md5=backup_md5、offset=0、`backup_used=true`，切换备用源重新下载；备用源也失败 → 仍走降级接受 + md5 告警。
- 未配置 backup_url → 原 sha256 降级路径不变（回归兼容）。

**验证**：`backup_failover.rs` 6 用例全绿——主源坏备源好（md5 恢复）/ 双坏（降级 md5 告警）/ 仅 backup_url 复用主源 sha256 / 无 backup 回归 / 主源好不触备源 / 无校验不触备源。`cargo test -p smart-dl-httpdl` 全量绿、clippy `--no-deps --all-targets -D warnings` 归零。

**边界**：切换后不递归（`backup_used` 防无限切换）；切换仅发生在「有校验目标」且「校验失败」的场景；无校验目标时不切换（与主源校验缺失语义一致）。

---

## 主线历史（已完成，详见 git log）

- 迅雷/QQDL 链接容错解码（`a989bb8`）、HTTP 断点续传 `.part`+etag（`0252c4f`）、BT fastresume 显式保存（`4f33cd1`）、任务持久化+恢复（`3ce222a`）、TOML 热重载（`784a269`）、HTTP 任务状态推进轮询（`9dea1a0`）、CLI 执行层（`3dfb8dd`）、端点补齐（`df82dfb`）等。
- **迅雷云盘线（F2/F2.1/F3…）外包专用会话，状态见 `BACKLOG.md` A 段。**