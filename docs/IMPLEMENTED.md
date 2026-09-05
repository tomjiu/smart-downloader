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
| C | 兜底传输撞上已抢先下完的同名文件时挂起 | **httpdl 侧已收口**：候选②`finalize_to` 幂等短路清理 .part、③`finalize_part` 删 dest 失败升级 warn 均已落地，新增 `dest_preexisting` 回归测试 4 用例全绿（crates/httpdl/tests/dest_preexisting.rs）；候选①`transition_for` 放行 Paused→Seeding 已在 daemon 侧实现（待其线提交） |

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
---

## 2026-08-30 批次（Phase 2：登录原生 UX / 能力吸收 / 主线增强）

### 14. 迅雷原生登录三模式（用户需求 Q1）

**行为契约**：
- `smart-dl-daemon xunlei-login`（默认 `--page`）：本地 `127.0.0.1:<随机端口>` 起登录页服务，页面复刻迅雷 App 登录视觉（深蓝渐变+白卡片+品牌标志+扫码/密码/短信三 Tab），扫码二维码由**本地模板**构造（`pan.xunlei.com/yc/?client_id=Xqp0…&user_code=…`，不依赖服务端 verification_uri）。
- `--browser`：系统浏览器直接跳转官方授权页完成设备码授权；本地保留备用登录页（同一会话）。
- `--qr`：终端 unicode 二维码 + 轮询状态行。
- `--token <path>`（默认 `./xunlei_auth.json`，POSIX 0600）；成功后打印 user_id，不回显 token。
- 登录态与 `XunleiProvider::new(_, token_path)` 完全互通（也兼容网页版 localStorage 凭证形状）。
- **DEVICE_CLIENT_ID 对齐**：`XW5SkOhLDjnOZP7J`（已知失败值）→ `Xqp0kJBXWhwaTpB6`（2026-08-25 实测通过值），常量注释留档变更依据；新增防回归单测。
- `Client` 支持 `with_bases()` 注入 mock 地址（测试可离线全链）。
- 密码/短信登录：`/v1/auth/signin`（captcha/init 全套签名 meta）与 `/v1/auth/verification{,/verify}` 编排进 `login_flow`；user_id 兜底从 JWT `sub` 解析。
- 离线下载 API 已有实现（offline_submit/offline_tasks/torrent_upload，Phase 1 交付）继续可用。

**验证**：`cargo test -p smart-dl-provider --lib` 102 全绿，含 `login_page_e2e_device_flow`（mock 上游 start→pending→authorized→落盘→读回）与 `login_page_password_flow`（captcha+signin→JWT 解 user_id→落盘）两个集成测试；`cargo check --examples -p smart-dl-provider` 全过；示例 `xunlei_qr_login.rs` 已切换本地 QR 构造。
**文档**：`docs/research/xunlei/NATIVE_LOGIN_GUIDE.md`（三模式使用说明/流程时序/复刻清单/合规声明）。

### 15. 能力吸收落地（用户需求 Q3）

**行为契约**：
- 协议嗅探引擎 `core/src/sniffer.rs`（FileCentipede 4 层规则移植）：scheme 直判（thunder/qqdl/fs2you/magnet/ed2k/ftp/http）、文本正则提取多链接、协议推断（.torrent 后缀、pan.xunlei.com/s/、pan.quark.cn/s/ 分享识别）、规则表可配置。
- BitComet 策略建议器 `core/src/strategy.rs` + `btcore::strategy` 门面：`DiskCacheAdvice`（自适应缓存+4 优先级桶，来源 r1 §4.3 + r2 disk_cache_priority）与 `AntiLeechAdvice`（分级反吸血→libtorrent settings_pack 建议值，来源 r1 §4.7）；纯函数，接入点注释标明。
- 夸克网盘 Provider `provider/src/quark/`：QuarkClient（stoken→detail→save→task→download 全链）、QuarkProvider 实现 `RemoteProvider`、Cookie 登录态持久化、错误分类（NotLogin/ShareExpired/QuotaExhausted）+ 失败冷却（同 xunlei 模式）。端点形状待真机验证（注释标注）。
- ed2k 链接解析 `core/src/source_parse/ed2k.rs`：解析 name/size/md4，路由层给出"已识别但暂不支持下载"的明确错误（完整引擎仍列远期）。

**验证**：sniffer 13 测 + strategy 7 测 + quark 10 测（axum mock）全绿；`cargo test --workspace --exclude smart-dl-btcore` 全绿。
**文档**：`docs/CAPABILITY_ABSORBED.md`（吸收能力总清单：✅/🔶/📋/🚫 四档逐项标注 + 不吸收决策清单 + 接入路线图）。

### 16. 跨平台通解文档（用户需求 Q2）

**产出**：`docs/research/xunlei/CROSS_PLATFORM_UNIVERSAL_SOLUTION.md`——分层通解判定（L0/L1 纯 Rust 全平台真通解 ✅ / L2 分平台等效通解 / L3 私有加速永不通解 ❌）+ 能力抽取矩阵（✅24 / 🔶11 / ❌7 项，逐项带仓库证据行号）+ 用户视角三平台通解矩阵 + macOS/Android 路线图 + 合规声明。

### 17. Linux/CI 编译修复

**行为契约**：`xunlei-ffi` Windows-only 代码全部 cfg 门控（非 Windows 编译为安全 stub）；`btcore/build.rs` 在无 libclang 环境自动回退到仓库内已提交 bindings.rs（剥离平台相关布局断言写入 OUT_DIR，`rustc-cfg=lt_bindings_fallback` 切换 include），`cargo check --workspace` 在 Linux 全绿。

### 18. 缺口解锁批次：fs2you 解码 / VIP 通道未测试代码 / cid_store 假设解析器（2026-08-30）

**背景**：用户追问「永不可做 7 项能做吗」→ 附录 A 重审（APP_COVERAGE_GAP_2026-08-30.md）后用户指示：「能做到的可以做了；#3/#4 无会员账号，先落未测试代码未来做完整」。

**行为契约**：
- **fs2you:// 解码（缺口 #1 ✅ 完成）**：`core/src/source_parse/fs2you.rs` —— base64 → `cachefile://host/path|size|md5` 三段解析（容忍 `cachefile://` 前缀缺失、scheme 大小写、宽松 b64 补齐），产出直链 + size + md5 元数据；normalize 路由 → Http 直链主流。10 个单测 + 2 个 normalize 集成测。
- **VIP 加速通道客户端（附录 A #3/#4 代码就位·UNTESTED）**：`provider/src/xunlei/vip_speedup.rs` —— `VipSpeedupClient`：`check_status`（✅ 响应形状已实测验证 SPEEDUP_SYSTEM §三，含 data 包裹双兼容）+ `try_speed_get_info/apply`（🔶 形状假设：官方桌面 inner-api 路由，trial_left_times/trial_key 等 Go json tag 同构）+ `speed_cert_res_status`（🔶 形状假设，产出 → `identity.set_accelerate_certification` A 级已封装）。三基址可注入，Bearer 票由调用方注入（登录态解耦）；风控拒绝原样透传不重试。8 个 axum mock 测试。
- **B 级 DCDN/VIP 凭证注入 FFI（附录 A #4 封装完成·UNTESTED）**：`xunlei-ffi` bindings/loader/identity 追加 `XL_EnableDcdnWithToken/Session/VipCert`、`XL_SetTaskEquityToken` 四导出 —— 形状来自反编译（§2.5），loader 以 **Option** 解析（版本缺失导出不中断 SDK 加载），封装层缺符号返回可读 DllLoad 错误；CString 封装纯函数单测。**首次真机调用前必须 dump 校准两个 c_int**。
- **cid_store.dat 假设解析器（附录 A #7 解封·HYPOTHESIS）**：`xunlei-convert/src/cid_store.rs` —— 三形态自适应探测（JSON / XDLCTX 同族 TLV / 裸二进制启发式：不可打印随机块{16,20,32}B × 相邻路径串 ASCII/UTF-16LE 配对，最小 gap 贪心消解 tag 对齐歧义）；`scripts/research/xunlei/cidstore_scan.py` 结构扫描器（隐私脱敏报告，样本到达后校准 Rust 侧）。4 个单测（含垃圾输入零 panic）。

**验证**：`cargo check --workspace` 全绿；core 100 / ffi 21 / convert 17 / provider 110(+ex 6) / daemon 44 / httpdl 10 全绿 —— 本批次新增 23 测。

**状态声明**：fs2you = ✅ 可用；VIP 通道与 B 级 FFI = 代码就位·UNTESTED（等用户试用/VIP 票据的真机会话校准，届时一次会话打通 get_info→apply→cert→FFI 注入全链）；cid_store = 假设解析器·待真实样本（`%APPDATA%\Thunder Network\cid_store.dat`，隐私口径见 sample_collection_guide）。

### 19. B-1 magnet → .torrent 元数据抓取（B 线第 1 项，2026-08-30）

**背景**：主线缺口盘点（§一·未完成表）第 1 项，用户批准 B 线开工首选。magnet 建任务后只能盲等
libtorrent 抓 metadata；缺「先抓元数据预览（文件清单/大小/tracker），再决定建不建任务」的入口。

**四层交付**：
- **core（纯 Rust，Linux 可测）**：`source_parse/magnet.rs` —— magnet URI 解析（v1 40 hex 强制、
  hybrid magnet v1 优先、v2-only 显式拒绝、percent-decode 含 UTF-8/dn 的 `+`→空格宽容、tr/ws 去重保序、
  非法输入零静默）15 测；`torrent_meta.rs` —— .torrent 字节 → 摘要（name/total/piece/files/trackers/
  url-list/comment/created_by + SHA1(info dict 原始字节) infohash，嵌套 span 定位自带实现）9 测。
- **FFI 一函数**：`lt_metadata(s, ih, buf, cap, out_len)`（lt.h + lt_kernel.cpp）——
  `create_torrent(ti).generate()` → bencode；内存契约同 resume/read_piece（Rust 预分配 + cap 不足
  BUFFER_TOO_SMALL + out_len）；metadata 未就绪 → NOT_FOUND（err_str 区分「任务不存在/未收到」）。
- **btcore**：`ffi::Session::metadata`（NOT_FOUND→Ok(None)，64 KiB 起步自动扩容）→ `BtCore::metadata`
  → `magnet.rs::fetch_metadata(magnet, scratch, opts)` —— 专用临时 session（与下载 session 隔离）
  → resume → bootstrap peers/追加 tracker 注入 → 轮询 metadata_received（timeout/ERROR 语义清晰）
  → 导出 → 摘要解析 + **infohash 引擎 vs 摘要交叉校验** → remove(delete_data) 清理。
  FetchOpts：timeout/extra_trackers/bootstrap_peers/enable_dht/poll_interval。
- **daemon**：`POST /bt/metadata`（feature bt 双态：无 bt 恒 400 提示编译开关；bt 下单并发 409）
  —— 入参 magnet/timeout_s(5..600)/dht/trackers/peers/save_to；出参 JSON 摘要 + torrent_b64 +
  saved_to；错误映射 400 坏 magnet / 408 超时 / 500 引擎。

**测试**：core 24 新测（magnet 15 + torrent_meta 9）；btcore e2e `magnet_metadata.rs`（本地 seeder
直连，双测：fetch_metadata 全链 + BtCore::metadata API 轮询语义，Windows LT 门禁跑）；
daemon `bt_metadata_api.rs`（无 bt：恒 400；有 bt：坏 magnet 400 / 坏 peer 400 / 不可达 infohash 408）。
顺带修 `http_api.rs` bt 构建 E0384（存量：cfg(bt) 分支重赋值不可变绑定）。

**验证**：core 124 / provider 115 全绿；daemon 四 feature 组合（默认 / bt / nas,ftp,xunlei-import /
webseed）编译零新增警告；非 bt daemon 测试 17 全绿。

### 20. 任务级顺序下载（sequential / 边下边播，2026-09-02）

**背景**：CAPABILITY_MAP 净增量 N3 的 httpdl/BT 侧落地。此前 HTTP 引擎 FIFO 领取段但
worker 无限并发在飞（clamp(total/64MB,2,8)），前缀完成速率被后段乱序完成拖累；BT 侧
FFI `lt_set_sequential` 已备但 daemon 层零接线（无入口、无持久化）。用户建任务后无法
表达「边下边播」意图。

**行为契约**：
- **任务字段**：`DownloadTask.sequential: bool`（serde default false；旧 tasks.json 零
  迁移；false 不序列化）。持久化 + 恢复重放（restore_from ③：sequential=true 原样下发，
  失败记事件不阻断恢复；flag 幂等）。
- **HTTP 引擎**：`download_dynamic` 新增 `sequential` 参数 → 在飞段窗口收紧到
  `SEQUENTIAL_WINDOW=2`（tokio Semaphore，permit 从领取前持有到 complete 后，RAII
  无泄漏；先拿 permit 再领取段）。FIFO 领取语义不变，仅收紧 lookahead。生效时机：
  新建任务立即；运行中任务下一次重下轮（换源/校验失败/续传轮）拾取。
- **BT 引擎**：daemon `BtEngine`（trait impl）新增 `set_sequential` → btcore（已备）
  → FFI `lt_set_sequential`（2.0.x = torrent_flags on/off；2.1 = set_sequential_range
  仅 on）。handle 级 flag：metadata 未就绪也可设；add_bt_task_opts/add_torrent_task_opts
  在引擎 add 后立即下发（失败仅记日志不回滚，与限速同口径）。
- **API**：① `POST /tasks` 请求体新增 `sequential: bool`（缺省 false；HTTP/FTP/magnet/
  .torrent 全链路）；② `POST /tasks/:id/sequential {"sequential": bool}` 任务级切换
  （404 任务不存在 / 409 引擎不支持即 FTP / 500 引擎错误；成功返回快照）。
  快照新增 `sequential`（false 不序列化）。
- **FTP**：不支持（Unsupported → 409），FtpEngine 走 trait 默认实现。

**测试**（8 新增）：
- httpdl `sequential_window.rs` 3 测：①自包含流式服务器（公式内容 i%251 现算，192MB
  仅落盘不驻内存——规避沙盒/CI OOM）直调 download_dynamic：顺序模式在飞峰值 ≤2；
  ②默认并行峰值 ≥3（worker 数）；③engine 接线冒烟（task.sequential → 完成 + 内容一致）。
- daemon `sequential_api.rs` 4 测：add 带 sequential 快照透出；端点切换 + tasks.json
  持久化（轮询落盘）+ 切回 false 字段缺失；404；恢复重放 e2e（restore_from 后
  sequential 保持 true）。
- daemon `bt_api.rs` 2 测（feature bt）：真实 torrent FFI flag 往返（端点 200 = 
  lt_set_sequential 真实生效，错误上抛 500）+ add 带 sequential 双任务（不同 name 防
  409 撞车）；magnet 假 btih（metadata 永不到达）flag 可设 → 200。

**验证**：非 bt workspace **631/631**（基线 624 + 7）；bt 构建 daemon **183/183**
（含 bt_api 16）；btcore 33 全绿；fmt 全清；clippy 非 bt workspace 与 daemon(bt)
--all-targets 零新增警告。

### 21. 常规能力增强批次 E1–E33（2026-09-02 ~ 09-04，PR #22–#57 全部合并）

**背景**：22 项愿望清单（5 梯队）逐项落地 + 事件面三通道 + 速率全链路真实化。
方向约束：只做固有常规能力（HTTP/BT/daemon API）增强，排除迅雷新方向。全部按
「实现 → 测试 → fmt → clippy 五门禁 → 独立 PR → CI 三 job（ubuntu/windows/bt
integration）全绿 → merge → 合并提交 check-runs 复核」流程逐批推进。

**批次总表**（批次 | 能力 | PR | 合并提交）：

| 批次 | 能力 | PR | 合并提交 |
|------|------|----|---------|
| E1+E2 | 并发探测加速 + 备用源兜底接线 | #22 | `9815302` |
| E3 | 校验失败隔离试错轮换 | #23 | `1139573` |
| E4 | Content-Disposition 文件名派生 | #24 | `e76d4a4` |
| E5 | 任务级代理（add 设定，仅 HTTP） | #25 | `1ab0018` |
| E6 | add API 能力对齐（headers/Basic 凭据/sha256/backup/显式 name） | #26 | `7b74842` |
| E7 | 任务管理面（过滤/分页/批量/delete_data） | #27 | `80f6b4b` |
| E8 | 任务级代理运行中热改（epoch 重入 + 段账本续传不断传） | #28 | `40aef31` |
| E9 | 任务名运行时回填（CD 派生链透出；#30 bt 编译修复） | #29 | `9d67568` |
| E10 | 事件历史缓冲 4096 + REST `GET /events` | #31 | `df88bda` |
| E11 | stats 速率真实化全链路（engine_status 缓存就绪） | #32 | `a577681` |
| E12 | SSE 事件流 `/events/stream`（#34 lint 修复） | #33 | `6840757` |
| E13 | 任务快照实时速率透出 | #35 | `aa20a43` |
| E14 | 任务搜索 `?search=`（名字/URL 大小写不敏感子串） | #36 | `1601263` |
| E15 | 任务重命名 `POST /tasks/:id/name` | #37 | `402f20a` |
| E16 | 全局限速总阀门运行中热改 `POST /config/limit` | #38 | `581c6ca` |
| E17 | 完成通知 Webhook（`[webhook] url`） | #39 | `a3e687b` |
| E18 | 任务标签（`?tag=` 过滤 + `POST /tasks/:id/tags`） | #40 | `68c693b` |
| E19 | 条件批量 `select`（states/engines/tags/search 任一） | #41 | `9a12bf0` |
| E20 | 已完成任务自动清扫（`[cleanup]`） | #42 | `047867f` |
| E22 | Prometheus `/metrics` | #43 | `dd66e6a` |
| E21 | 文件冲突策略 overwrite/rename/skip | #44 | `b1d12ed` |
| E23 | 定时启动 `start_at_unix` + 错峰 jitter | #45 | `63ec9e7` |
| E24 | 多源并行（双源强 ETag 相等才跨源混拼） | #46 | `a600f6b` |
| E25 | 校验算法扩展 sha1/md5（与 sha256 互斥） | #47 | `e6960c6` |
| E26 | 断点续传双指纹加固（ETag + Last-Modified） | #48 | `9a026bc` |
| E27 | 完成后自动处理（move_to + hook，`[post_download]`） | #49 | `da5675e` |
| E28 | BT 任务名回填（magnet metadata 到达） | #50 | `3b5b944` |
| E29 | BT tracker 运行时管理（GET/POST/DELETE trackers） | #51 | `6914e04` |
| E30 | 失败自动重试预算（指数退避；#53 fmt、#55 post-hook flaky 修复） | #52 | `6d55dd8` |
| E31 | 探测预览 `POST /probe`（不建任务） | #54 | `78ed137` |
| E32 | 终态手动重试（resume 复用） | #56 | `1766696` |
| E33 | 上传/分享率统计（all_time 累计透出） | #57 | `0c5cb52` |

**行为契约要点**（跨批次交互语义，实操查阅用）：

- **事件面三通道**：WS（双向，背压保护）+ REST `GET /events`（seq 游标分页，
  task_id/type 过滤，`truncated` 缺口报警 = 应放弃增量改全量重同步）+ SSE
  `/events/stream`（历史重放 + 活流尾随，`Last-Event-ID` 断线续传）。三通道
  共用同一 Envelope 解析与 WsHub 环形缓冲（容量 4096）。
- **速率链路**：引擎侧采样 → daemon engine_status 缓存（E11）→ `/stats` 聚合
  与 `GET /tasks/:id` 快照 `rates{down_bytes_s,up_bytes_s}`（E13，0 值序列化
  省略）；BT 侧 libtorrent `all_time_download/upload`（FFI shim 扩展，E33）
  → `total_downloaded/total_uploaded/share_ratio`（3 位小数，down=0 → None
  省略）。注意：LT all_time 计数器在 session second_tick（≈1s）才冲账，进度
  到 1.0 后立即读恒 0——消费方需轮询等待。
- **任务名派生链**：用户显式 `name`（E6）> Content-Disposition（E4）> URL
  末段 > `download.bin`；BT magnet 任务 metadata 到达后回填（E28）；rename
  API（E15）覆盖显示名，`{"name": null}` 清除回退派生链。`?search=`（E14）
  语料 = 任务名 + 来源 URL（经 `search_urls` 脱敏），大小写不敏感子串。
- **代理**：任务级 `proxy`（E5，add 设定，仅 HTTP）+ 运行中热改
  `POST /tasks/:id/proxy`（E8）：epoch 重入 + 段账本续传，换代理不断传。
- **列表/批量**：`GET /tasks` 无参数完全兼容；`?state=`/`?engine=`/`?tag=`
  逗号分隔多值（维度内 OR、维度间 AND，大小写不敏感，合法标签从全变体生成）；
  `?limit=1..=500`/`?offset` + `X-Total-Count`；排序恒为 task_id 数值后缀升序。
  `POST /tasks/batch`：显式 `ids`（≤100，pause/resume/remove）或条件 `select`
  （E19，同 ListQuery 选择器，仅 pause/resume 非破坏动作）；单项失败不短路，
  恒 200 逐项回执。`DELETE /tasks/:id?delete_data=true` 引擎侧同删数据。
- **冲突策略**（E21，仅 HTTP 显式名任务）：`overwrite`（默认）/ `rename`
  （`name(1).ext` 起取首个空闲）/ `skip`（既有文件保持原样，任务直接
  Completed，完成事件/Webhook/钩子照发；post_download move_to 尊重 skip）。
- **定时/错峰**（E23）：`start_at_unix` 未来时刻到点前不入引擎（停留 Queued，
  pause = 取消定时，resume = 立即激活）；`[scheduler] start_jitter_seconds`
  仅在 start_at 缺省时叠加（0..=N 秒随机）。宽容语义：过去时刻不 400。
- **重试体系**：E30 自动重试 `auto_retry`（0..=10，越界 400；仅 HTTP/FTP），
  指数退避 2s/4s/8s…封顶 60s，预算耗尽落 Failed；E32 手动重试 = 终态 Failed
  任务 `resume`（重新接入引擎），**不重置** auto_retry 预算（防白给循环）。
- **校验**（E25/E6）：`sha256`/`sha1`/`md5` 三选一互斥（同时多个 → 400）；
  `backup_md5` 必须与 `backup_url` 成对。E3：校验失败隔离试错轮换（坏源
  退避，不烧备用源）。E26：续传前 ETag + Last-Modified 双指纹确认服务器
  文件未变，任一变化作废段账本重下（防跨文件拼接脏数据）。
- **多源并行**（E24）：仅当双源强 ETag 相等且 Range 支持与总长一致才启用
  跨源分段混拼（worker 轮转分摊）——严于 aria2 的无条件多源。
- **完成面**：E17 Webhook（fire-and-forget，单次 5s 超时，失败仅日志）；
  E27 `[post_download]`：`move_to`（同盘 rename，跨盘 copy+delete，同名自动
  改名）+ `hook`（不带 shell 直启，env 传 SD_TASK_ID/SD_TASK_NAME/
  SD_FILE_PATH/SD_ENGINE）；E20 `[cleanup]`：Completed 保留 N 天（0=禁用），
  清扫间隔 10min，`auto_remove_keep_data` 默认保留文件。
- **BT 专项**：E29 tracker 运行时 `GET`（announce 表）/`POST`（批量追加，
  非法 URL 400）/`DELETE ?url=`（精确匹配，无匹配 404）；E33 分享率见速率
  链路；发现层开关（DHT/LSD/UPnP）配置段早已有（见 #2 批次）。
- **运维**：E22 `/metrics` Prometheus 文本格式（任务按状态/引擎计数 + 聚合
  速率）；E31 `/probe`（GET Range: bytes=0-0 探测大小/服务端文件名/Range/
  ETag/Last-Modified/Content-Type + `suggest_name` 与引擎派生链一致，不建
  任务；v1 仅 HTTP 源）。E16 `/config/limit`：合计下行 + BT 上行，缺省字段
  = 沿用当前值，双缺省 = 查询。

**验证**：非 bt workspace 基线 624（sequential 批次时点）→ **850/850**（E33
收官，`cargo test --workspace --exclude smart-dl-btcore`）；bt 构建 daemon
322 + btcore 35（本地 LT 2.0.11，含真实 seeder 环回下载断言 all_time 冲账）；
fmt 全清；clippy 五门禁（workspace+ftp / daemon ftp,nas / ftp,nas / bt
--all-targets）零警告。每批独立 PR，CI 三 job 全绿后才合并，合并提交
check-runs 逐一复核（E13–E33 段：#35–#57）。

### 22. 技术债批次（2026-09-05，PR #61–#67）

技术债清单五项收官（清单背景见 2026-08-30 审计报告与 LOCK_MODEL.md）：

- **#1 锁模型 hardening**（PR #61）：消除唯一多锁同持边，锁序审计结论
  固化 `docs/LOCK_MODEL.md`。
- **#2 state.rs 三步拆分**（PR #62 → #63 → #65，纯移动零语义）：
  第一步测试区外置单文件（-54%）→ 第二步生产区按领域拆 `state/` 子模块
  （bt_alerts / lifecycle / ops / persistence，state.rs -86%）→ 第三步
  测试区目录化（`state_tests/` 一 mod 一文件 24 文件 + 外壳 mod.rs，
  路径零变化，glob 解析链同构）。state.rs 最终 636 行骨架。
- **#3 消 flaky**（PR #64）：根因 = libtorrent 2.0 session_params 默认
  监听 0.0.0.0:6881 且 ffi 无 settings 导出，同 binary 并行测试抢端口。
  解法 = `tests/common/lt_gate.rs` 进程内 tokio Mutex 串行门（跨 binary
  天然串行，无需文件锁），`scripts/insert_lt_gate.py` 函数级污染分析
  （BtEngine::new 为源 + 调用链传播）幂等插入 **83 处**
  （http_api 47 / bt_api 18 / fastresume 6 / fallback 5 / bt_metadata 4 /
  xunlei 3，纯逻辑测试自动跳过）。锁序约定：先 lt_gate 后 seeder 文件锁。
- **#4 FTP 分段对齐**（PR #67）：FTP 从静态 2-8 段顺序下载对齐到 HTTP
  直链的动态分段语义——SegmentManager FIFO（16MB 粒度，<16MB 单段）+
  JoinSet worker 池（segment_count 同公式）+ 段账本续传全链（.part 长度
  前缀语义废弃，账本唯一凭据）。契约测试：账本恢复只拉缺失段 / 多段并行
  完整性 / 损坏账本作废。
- **测试卫生**（PR #66）：CLI e2e add 测试注入独立 tempdir——消
  `crates/daemon/file` 仓库内垃圾（default_dest_root 缺省 "." 的副产物，
  「提交前必须 rm 测试垃圾」纪律即源于此）。
- **#5 BT 本地门槛**：`scripts/ci/bt-linux-setup.sh` 补 rustc 1.98 链接
  布局规避（linker-wrap.sh：剥 -fuse-ld=lld / -B gcc-ld /
  -nodefaultlibs + 本地前缀 -L 前置注入——no-root 场景实测必踩），
  本地搭建指引固化 `docs/BT_LOCAL_BUILD.md`。

**验证**：全口径 fmt --check 绿；clippy CI 口径（workspace+httpdl/ftp、
daemon ftp|nas|ftp,nas、btcore --all-targets、daemon bt --all-targets）
零警告；测试面 daemon default 264 / ftp,nas 275 / bt 322 / btcore 35 /
httpdl(ftp) 179 / lib 单元 134 全绿。每项独立 PR + CI 三 job 全绿后合并。
