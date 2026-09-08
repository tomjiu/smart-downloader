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
- **重试体系**：E30 自动重试 `auto_retry`（0..=10，越界 400；原仅 HTTP/FTP，
  2026-09-05 A2 起 BT alert 快路径同口径拦截，见 #24），
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

> **#22 补记（同日后续 PR）**：#68（BT 本地门槛文档化 `docs/BT_LOCAL_BUILD.md`，
> 上表已并入 #5 条目）/ #69（httpdl 存量 lint 清零：items-after-test-module 归位、
> mirror_failover 未消费绑定、task_proxy doc 续行缩进）/ #70（FTP 失败缩小
> 粒度重试对齐 HTTP + RETR 过量发送死锁修复：读满子段配额即 drop，服务器
> EPIPE 收尾；测试基建 fail_ranges 注入）——三者均已合并，全口径门禁同上。

### 23. BT 传输/发现配置面补全：PEX / uTP / MSE 加密三态（A 档快赢 #1，2026-09-05）

**行为契约**：
- 配置面 `[bt]` 新增三键（均启动时一次 apply，不参与热重载）：
  - `enable_pex`（默认 false）：PEX（peer 交换）。**内核特殊处理**：libtorrent
    2.0.x 无 settings_pack 会话级 PEX 开关（默认开），`lt_apply_discovery` 置 0 时
    会话记录意图（`lt_session::pex_disabled`），对**其后新增任务**（magnet/.torrent/
    resume 三入口）注入 per-torrent `disable_pex` flag；不回溯既有任务（daemon 在
    任务装配前 apply，故覆盖全部任务）。**行为变化**：daemon 默认 PEX 由内核默认
    开 → 配置默认关（对齐 M0「发现层默认全关」与私有 tracker 友好）。
  - `enable_utp`（默认 false）：uTP 双向开关（`enable_incoming_utp`/
    `enable_outgoing_utp` 同进退），保持 v1 内核「默认纯 TCP」决策。
  - `encrypt`（默认 `allow`）：MSE（Protocol Encryption）三态——`disable` 纯明文
    （拒 MSE）/ `allow` 明文+加密皆收（内核默认行为不变）/ `require` 强制加密
    （明文直接拒）。映射内核 `in/out_enc_policy`（pe_disabled/pe_enabled/pe_forced）；
    `allowed_enc_level`/`prefer_rc4` 不暴露（保持 pe_both/false）。
- 契约层：`ffi/lt.h` 新增 `lt_apply_transport(session, enable_utp, enc_policy)`；
  `lt_apply_discovery` 扩参第 4 位 `enable_pex`；bindings.rs 提交版手动同步。
- btcore：`ffi::EncryptPolicy`（repr(u32) 0/1/2 对齐内核三态）+ `engine::
  parse_encrypt_policy`（disable/allow/require，前后空白容忍）；`BtCore::
  apply_discovery(4 参)` / `apply_transport`。
- daemon：`BtCfg` 手动 Default（encrypt 缺省必须 "allow" 而非空串；字段级
  `serde(default = "default_bt_encrypt")` + 容器级 serde(default) 共存）；
  `Config::load` fail-fast 校验（非法值启动即报错，白名单与 btcore 同口径——
  btcore 是 feature 门控依赖，config 层本地同步）；`BtEngine::new` 扩参 10 参
  （`#[allow(clippy::too_many_arguments)]`）；`/config` 快照新增
  `bt_enable_pex` / `bt_enable_utp` / `bt_encrypt` 三键。
- `config.toml.example` [bt] 段同步三键注释。

**验证**：btcore 37/0（基线 35 + `apply_transport_smoke_roundtrip`（uTP 两态 ×
加密三态全组合）+ `parse_encrypt_policy_variants`）；daemon default 266/0、
bt --all-targets 324/0、ftp,nas 277/0（各基线 +2：`bt_transport_toml_overrides`
（含 encrypt 缺省回填 allow 断言）+ `bt_encrypt_invalid_rejected`（4 非法值 +
空白容忍））；fmt/双 clippy 口径 0 告警。G1 手动脚本不受影响（对照 run 全关
语义不变）。

### 24. auto_retry 覆盖 BT 任务（A 档快赢 #2，2026-09-05）

**行为契约**：
- E30 失败自动重试此前仅拦截 HTTP/FTP：轮询路径（ops poll snapshot → `fail_or_schedule_retry`）
  与引擎 add 失败路径均引擎无关，但 **BT alert 快路径**（`apply_bt_alert` 收到
  State:Error alert）直写 Failed 绕过重试——alert 先到即定终，轮询兜底拦截形同虚设。
- 本次对齐：`apply_bt_alert` 对 Error alert 命中**活跃任务**（Queued/Downloading）时
  经 `fail_or_schedule_retry` 拦截——预算未用尽 → 清引擎句柄回 Queued 安排指数退避
  （调度循环 `activate_due_tasks` → `activate_one` 本就引擎无关，BT 经
  `engine_for(Bt) + add` 重接入，复用 fastresume）；预算用尽 → Failed 终态。
- **活跃态门控**与轮询路径守卫一致：仅 Queued/Downloading 拦截；Paused/Seeding 下的
  Error 保持旧直终语义（暂停任务不得被重试悄悄复活；做种失败不自动重下）。
- 广播与可观测：BtAlertEffect.to 为拦截后实际目标（Queued，非引擎报的 Failed）；
  `engine_status.error` 按引擎原始去向记录（重试排队也保留最近失败原因）；
  停转速率清零扩展到重试排队（E11 同源，防 /stats 陈旧速率虚高）。
- 持久化：重试安排（next_retry_at_unix）随 TaskMetadata autosave 落盘，重启恢复
  继续等待（E30 既有语义，BT 无新增持久化面）。

**验证**：bt_alert_tests 新增 4 测——`error_alert_within_budget_schedules_retry`
（拦截 → Queued + 预算消耗 + 句柄清空 + 退避安排 + auto_retry 事件 + error 保留 +
速率清零）/ `error_alert_exhausted_budget_is_terminal` / `error_alert_on_paused_
stays_terminal` / `error_alert_on_seeding_stays_terminal`；既有
`error_alert_fails_with_message`（预算 0）不变绿。btcore 37/0 / daemon default
266/0 / bt --all-targets 328/0（+4）/ fmt / workspace clippy 0 告警。

### 25. FTP 任务级限速 + 顺序下载补齐（A 档快赢 #3，2026-09-05）

**背景**：任务级限速（E16）与顺序下载（sequential，N3）此前为 HTTP/BT 双引擎
实现，FTP 明示「不支持」（`EngineInner` 注释 + `task.rs` 字段注释）——FTP 任务
无法单独限速、边下边播窗口不可用。

**行为契约**：
- **任务级限速**（`set_limits`，与 HTTP 引擎同口径）：任务专属 limiter 登记
  `limiters` 表（`RateLimiter::new_chained` 串联引擎全局上游——总阀门对任务级
  限速任务同样生效）；已登记 → 原地热调（运行中任务经 Arc 共享即时生效）；
  未登记（含运行中走全局的任务）→ 新登记，下一次重下轮拾取。`up` 方向显式
  拒绝（Other，FTP 无上传）；缺失任务 NotFound；remove 时登记一并回收。
  下载循环 spawn 取用口径与 HTTP 相同（limiters.get 优先，回退全局）。
- **顺序下载**（`set_sequential` + `task.sequential` add 直读）：复用 HTTP 的
  在飞闸门模式——`SEQUENTIAL_WINDOW`（=2）Semaphore，permit 从领取前持有到
  complete 后释放（RAII，失败/panic 无泄漏）；「先拿 permit 再领取段」保证
  窗口语义（先领后等会让窗口外表内的段占用 FIFO 游标）。FTP 目录任务按文件
  串行（既有语义），sequential 收紧的是文件内段在飞窗口。运行中热改 = 字段
  回显（FTP 单轮下载，实际生效点 = add / 恢复重放）。
- `task.rs` sequential 字段注释同步（FTP = 支持，与 HTTP 同值同语义）。

**验证**：新增 `tests/ftp_task_controls.rs` 4 测——up 拒绝 + NotFound / 限速
双路径（insert+热调）下完成且内容一致 / sequential 多段（16KB 粒度 ×4 段）
窗口下完成且内容一致（retr≥4）/ set_sequential 回显 + NotFound。门禁：
httpdl(ftp) 184/0（+4）· daemon default 266/0 · ftp,nas 277/0 · bt 328/0 ·
fmt · 双 clippy 口径 0 告警。daemon `/tasks/:id/limit`、`/tasks/:id/sequential`
端点走 trait 分发，FTP 支持自动生效（无需 daemon 改动）。

### 26. `/metrics` 任务速率 histogram（A 档快赢 #4，2026-09-05）

**背景**：E22 的 `/metrics` 仅有 gauge 面（任务计数 + 聚合速率），无分布视图
——多任务并发时聚合值掩盖单任务速率形态（限速是否生效、慢尾任务占比）无法
观测。

**行为契约**：
- 新增两条 histogram（text/plain 0.0.4，Prometheus 抓取兼容）：
  - `smart_dl_task_down_speed_bytes_per_second`（单任务下载速率分布）
  - `smart_dl_task_up_speed_bytes_per_second`（单任务上传速率分布，BT 做种观测）
- 按 `engine` 标签分独立 series（bt/http/ftp/provider/xunlei-nas，BTreeMap
  输出序稳定）；桶边界 64KB/256KB/1MB/4MB/16MB/64MB（bytes/s，累计口径，
  渲染器补 +Inf/sum/count）。
- **样本口径**：仅含任一方向速率 > 0 的任务（`task_speed_samples()`）——
  count = 当前传输中任务数，空闲任务堆积不灌爆低桶；各方向 histogram 只取
  该方向速率 > 0 的样本（BT 纯上传任务不进 down 分布）。
- `/stats` JSON 面保持既有字段不变（histogram 为 /metrics 专属派生）。

**验证**：http.rs 渲染单测 4 测（累计桶/零样本排除/方向列选取/空样本仅
HELP+TYPE/engine 序稳定）+ state_tests/metrics_tests 2 测（全零与无缓存排除/
空态）。门禁：daemon default 272/0（+6）· ftp,nas 283/0（+6）· bt 334/0
（+6）· fmt · workspace clippy 0 告警。

### 27. cookie jar（reqwest cookies，A 档快赢 #5，2026-09-05）

**背景**：HTTP cookie 仅能经任务级自定义头手工携带（H-8）——登录型源
（Set-Cookie 会话）探测通过后段请求 403，用户必须自己抓 cookie 填头；
重定向链上的会话 cookie 也无法自动维持。

**行为契约**：
- workspace reqwest 启用 `cookies` feature；两处下载链路 client 启用
  `cookie_store(true)`：
  - daemon serve 引擎共享 client（serve.rs）：内存 jar，同站跨任务共享
    （浏览器会话语义——登录一次全站任务通吃）；
  - httpdl `build_proxied_client`（任务级代理 client）：每任务独立 jar，
    与全局 client 隔离。
- 语义：探测/段请求/重定向自动携带与更新 cookie；Set-Cookie 自动入 jar；
  任务级 Cookie 头仍可显式覆盖（H-8 通道保持，两者叠加时服务端以后到为准）。
- 边界：`/probe` 端点的单发请求 client 与 NAS 管理面 client 不启用（一次性
  请求 jar 无意义/管理面非下载链路）。

**验证**：新增 `tests/cookie_jar.rs` e2e——服务端首个无 cookie 请求（add
探测）为引导请求（200/206 + Set-Cookie），其后任何无 cookie 请求 403 计数；
断言任务 Completed + 内容一致 + cookie 请求 ≥1 + 引导后无 cookie 请求 == 0
（jar 确实在探测→下载链路传递）。门禁：httpdl(ftp) 185/0（+1）· daemon
default 272/0 · bt 334/0 · fmt · 双 clippy 口径 0 告警。

### 28. Metalink4 支持（B 档 #1，2026-09-05）

**背景**：多镜像发布场景（Linux 发行版 / 开源大文件）用户需手工挑一个 URL
逐个添加，无镜像聚合描述文件展开能力；aria2/motrix 均原生支持 metalink。

**实现**（`daemon/metalink.rs` + add 链路接入，PR #76）：
- **解析器**：quick-xml 0.37 事件流状态机（RFC 5854），按 local name 匹配
  标签（默认命名空间 + 带前缀 `<ml:file>` 均兼容）；`<file name=/>` `<size>`
  `<hash type=/>` `<url priority= location=>` 全字段收集；`sha-256`/`sha-1`
  连字符变体归一化；文档级元数据（publisher 等）忽略；`<size>`/priority
  非法数字 → Err（拒静默吞错，防破坏校验/预检语义）。
- **展开语义**（`add_metalink_tasks`）：逐 `<file>` 展开为一个 HTTP 任务——
  主 URL = priority 最高（数值最小，None 排后稳定排序），次高者作 backup_url
  （E2/E3 mirror failover 直通）；内建哈希择强（sha256 > sha1 > md5）直通
  E3 校验链（主备同内容，backup_md5 恒不设）；文件名取 name 属性末段
  （子目录展开 v1 不做，`display_name` 过 `sanitize_rel` V3 终审，穿越段
  随「仅取末段」语义天然丢弃）；仅 http(s) URL 参与（ftp:// 等混合协议
  过滤，v1 无 FTP 任务生成面）。
- **API 三选一**（`POST /tasks`，优先级 torrent > metalink > url）：
  ① `metalink_b64`（本地 .meta4/.metalink 内容，UTF-8 XML）；
  ② `url` 以 `.meta4`/`.metalink` 结尾（剥 query/fragment 大小写无关）→
  daemon bootstrap client 引导拉取（serve 注入引擎全局 client 克隆——
  全局代理/cookie jar/超时同源；未注入时裸 client 兜底）→ 展开任务集；
  ③ 常规 url 照旧。响应统一 `task_id`（首个，向后兼容）+ `task_ids` +
  `count`；任务级字段（sequential/proxy/headers/auth/conflict/start_at/
  auto_retry/limits）逐文件继承。
- **失败语义**：XML 解析错误/文件无可用 http(s) URL → 400（含文件名定位）；
  展开中途失败即中止，已创建任务保留不回滚（错误信息带已创建计数）。

**验证**：`metalink.rs` 单测 10（字段/排序/择强/命名空间前缀/转义/多文件/
混合协议过滤/坏输入/元数据忽略/末段语义）+ `tests/metalink_api.rs` e2e 3
（metalink_b64 展开+sha256 校验+failover、.meta4 URL 引导拉取、坏 XML 400）。
门禁：daemon default 285/0（+13）· ftp,nas 296/0（+13）· bt --all-targets
347/0（+13）· httpdl(ftp) 185/0 · btcore 37/0 · fmt · CI 双 clippy 口径 0 告警。

### 29. FTPS 支持（B 档 #2，2026-09-05）

**背景**：FTP 引擎仅明文传输，凭据（USER/PASS 明文过网）与数据均无加密；
主流 FTP 服务（FileZilla Server / proftpd / vsftpd TLS 启用）均提供显式
FTPS，aria2/wget/curl 原生支持。

**实现**（RFC 4217 显式模式，PR #77）：
- **`ftps://` 全链识别**：core `parse_ftp_auth`（scheme 无关剥离，auth 段
  同构）+ `normalize_user_link`（ftps → NormalizedSource::Ftp）+ daemon
  `add_ftp_task` 前缀校验放宽；默认端口仍 21（显式 AUTH TLS 惯例，与
  FileZilla/wget 一致；隐式 990 后续按需）。
- **控制连接升级**（httpdl `FtpSession::connect(use_tls)`）：明文 banner →
  AUTH TLS（234）→ TLS 握手（tokio-rustls 0.26/ring，与 reqwest rustls-tls
  同一 rustls 0.23 编译单元；rustls 栈经 re-export 使用零新增版本）→
  PBSZ 0 → PROT P；任一步被拒即连接失败。传输流抽象 `FtpIo` trait object：
  明文 TcpStream 与 TlsStream 同一读写口径（read_response/write_cmd 泛型化）。
- **数据连接 PROT P**：PASV 后 TcpStream connect → 立即 TLS 握手（SNI 同
  控制连接），download_segment/probe_list 全数据路径覆盖；PROT C（明文
  数据）不支持。
- **证书校验严格开启**：webpki-roots 固定根集（v1 不暴露 insecure 口子；
  自签/私有 CA 场景后续按需加自定义根集配置面；测试经 `#[cfg(test)]`
  TEST_CONNECTOR 注入钩子，生产零影响）。
- TLS 依赖挂 `httpdl/ftp` feature 的 optional deps（非 ftp 构建零开销）。

**验证**：内嵌 FTPS 服务器 e2e（rcgen 自签证书 + tokio-rustls server，
RFC 4217/959 最小协议循环）——download_segment 全路径（AUTH TLS 升级 +
PBSZ/PROT P + 数据连接 TLS + REST/RETR）落盘内容逐字节断言；解析单测
（parse_ftp_url ftps 标志/默认端口、parse_ftp_auth ftps 同构、normalize
ftps 分类）。门禁：httpdl(ftp) 187/0（+2）· core 255/0（+2）· daemon
default 285/0 · ftp,nas 296/0 · bt 347/0 · fmt · CI 双 clippy 0 告警。

### 30. HLS（m3u8）下载支持（C 档 #1，2026-09-05）

**背景**：流媒体点播（VOD）场景 `.m3u8` 清单只能用播放器播放，无下载器
把"清单 → 分段拉取 → 解密 → 合流"链路自动化；aria2/ffmpeg 各覆盖一半
（aria2 无解密合流、ffmpeg 无断点续传/限速/任务管理）。

**实现**（RFC 8216 子集，PR #78）：
- **识别与分流**：`HttpEngine::add` 对 URL 路径 `.m3u8` 后缀（剥 query/frag
  大小写无关）的任务分流 HLS 路径——不走 Range 探测/分段链（清单是文本，
  total/Range 语义无意义）；任务级代理/headers/限速全链同源。
- **清单解析**（`hls.rs`）：master playlist 变体 → BANDWIDTH 最高者展开
  一层；media playlist 解析 EXTINF/EXT-X-MEDIA-SEQUENCE/EXT-X-KEY/
  EXT-X-ENDLIST；**仅接受 VOD**（无 ENDLIST = live → 任务 Error：清单随
  时间变化续传语义不成立）；EXT-X-BYTERANGE / EXT-X-MAP / SAMPLE-AES →
  明确不支持报错。
- **AES-128-CBC 解密**（RustCrypto aes+cbc）：EXT-X-KEY 的 key URI 拉取
  一次缓存（同 key 多段共享）；IV 显式声明或缺省 = 段 media sequence
  大端 16B 推导；PKCS7 unpadding（每段独立密文单元）；密钥行内状态切换
  （同清单多 key 段混布支持）。
- **顺序下载 + 段账本续传**：段顺序 append `.part`（TS 拼接对顺序敏感，
  v1 不并发段）；`.part.hls-ledger` JSON 记录清单指纹（sha256 playlist
  文本+URL）/已完成段数/字节数，每段落盘即记账；恢复 = 重拉清单 → 指纹
  对账（失配作废重下）→ 从断点段续传，已完成段零重复请求（e2e 计数断言）。
- **pause/resume**：abort flag 段间检查点（`hls-aborted` 约定错误静默
  退出，状态由 pause()/remove() 管理）；resume epoch+1 重 spawn 凭账本
  续传——与 HTTP 段账本同模式。
- **落盘名**：显式名权威；派生 = 入口 URL 清单名去 `.m3u8` + `.ts`
  （变体运行时展开，media 内容合流进同一交付文件）；total = 0（段长
  事先未知，BT metadata 前同"未知长度"语义），done 按字节累计。
- 依赖：aes 0.8 + cbc 0.1（RustCrypto 纯 Rust 小依赖，无 feature 门）。

**验证**：解析单测 12（VOD/AES 显式+缺省 IV/live 拒绝/BYTERANGE/MAP/
SAMPLE-AES 拒绝/结构非法/master 选流/URL 解析/识别/落盘名/AES roundtrip）
+ e2e 4（master 展开+2 段解密逐字节、段账本续传零重复拉取、live 任务
Error、pause→resume roundtrip）。门禁：httpdl(ftp) 203/0（+16）· core
255/0 · daemon default 285/0 · ftp,nas 296/0 · bt 347/0 · fmt · CI 双
clippy 0 告警。

### 31. 百度网盘分享解析（B3-a，2026-09-05）

**范围**：分享链接 → 免登录文件清单（verify → BDCLND → 分享页 meta → share/list）；
dlink 直链转换需登录态（实测 errno -6），归 B3-b 待 BDUSS 真机校准。
协议证据：`docs/research/baidu/share_protocol.md`（A 级，真实链接 curl + Rust 双验证）。

**实现**（`crates/provider/src/baidu/` + CLI）：
- `share.rs`：`parse_share_link` 双形态统一规约——`/s/1<code>`（去 `1` 前缀存 code）
  与 `/share/init?surl=<code>`；提取码经 url::Url 保留原始大小写（百度大小写敏感）。
- `client.rs`：`BaiduClient`——verify **POST**（GET 同参数实测 errno -12 风控；
  body `pwd=`，Referer=init 页，app_id=250528）→ randsk 种 BDCLND（host-only）；
  分享页 HTML 提取 shareid/uk（JS 赋值 `shareid:".."` 与 JSON 字符串两形状，
  `"uk":0` 数字噪声以引号值形状匹配规避）；`/share/list` root=1 / dir= 双形态，
  字符串数字字段 `de_string` 兼容（实测 size/isdir/fs_id 均字符串）。
- `types.rs`：`BaiduError` 分类（WrongPasscode/-12、NeedVerify/9019、MetaParse、Protocol）；
  UA/app_id 常量（实测值）。
- CLI：`smart-dl-daemon baidu-resolve <url> [--pwd CODE] [--dir /path] [--json]`
  （main.rs 分发前拦截，本地执行不依赖 daemon 进程；同 xunlei-login 模式）。

**行为契约**
- 短时间连续 verify 会触发百度风控（分享页退化为无 shareid 风控页，数分钟自愈）→
  `MetaParse` 归类（"分享可能已失效或风控"）。
- 公开分享（无提取码）跳过 verify 直取 meta。
- dlink/转存/RemoteProvider 契约接入 = B3-b（BDUSS cookie 配置面一并做）。

**验证**：provider baidu 单测 14（解析 7 + HTML 提取 3 + axum mock 全流程 4，
mock 形状与实测协议一致）+ daemon baidu_resolve 1 + CLI parse；
**真实链接 e2e**：`baidu-resolve` 对用户真实分享直连成功（提取码校验 →
shareid/uk → 根目录 + `--dir` 子目录清单 + `--json`）。门禁：fmt · CI 双
clippy 0 · httpdl(ftp) 203/0 · core 255/0 · btcore 37/0 · provider 162/0 ·
daemon default 286/0（+1）· ftp,nas 297/0（+1）· bt --all-targets 348/0（+1）。

## S1 设置面 + S2 前端入仓 + 桌面端（2026-09-06 批次）

### 1. 运行时设置 API（`GET/PUT /settings`，feat 提交 934a700）

**API**：
- `GET /settings`：十域快照——`bandwidth`（基准限速 + 备用调度 + 引擎实际
  生效值 `effective_*` + 窗口命中态 `alt_active_now`）/ `connection`
  （proxy + bt_listen_port + bt_max_connections）/ `bittorrent`（发现五键 +
  encrypt 三态 + bt_available）/ `download` / `cleanup` / `post_download` /
  `webhook` / `scheduler` / `queue` / `meta.persist_path`。
- `PUT /settings?persist=`（缺省 true）：域级部分更新；**验证前置零副作用**
  （encrypt 白名单 / HH:MM 格式 / alt_days∈[0,6] / proxy scheme 白名单 +
  主机段非空 / dest_root 非空，违规 400）；成功响应
  `{applied[], restart_required[], persisted, limits}` + `settings_changed`
  事件（daemon 级，keys 点分全集）；限速变更沿用 `global_limits_changed`。
- 持久化：权威配置（live_config：启动注入 + 热重载跟随）打补丁 →
  `Config::save_to` 原子回写（tmp+rename；注释不保留为文档化边界）；
  `--config` 未指定时 persisted=false 仅运行时生效。与热重载天然协同
  （回写后 5s 轮询按文本变更检测读取同值，幂等 no-op）。

**生效语义**（每域文档化）：带宽 立即（E16 同链路，双轨：基准 base_limits /
引擎实际 global_limits）；代理 BT 立即（settings_pack 全量重放）/ HTTP 新任务
（逐任务 client 构建合并运行时全局，任务级代理仍优先，存量任务不受扰）；
BT 发现/传输/连接 立即；dest_root 立即（目录创建 + 白名单追加）；cleanup/
post_download/webhook/jitter 立即；disk_precheck_strict 重启；queue 持久化
预留（S1-b 门控接线见 BACKLOG）。

**备用限速调度**：`[limits] alt_enabled / alt_max_*_kb_s / alt_from / alt_to /
alt_days`；窗口 `[from, to)` 半开 + 跨零点回卷（from>to）+ 星期过滤
（空=每天；0=周日）；`alt_window_active` 纯函数单测直打；serve 30s ticker +
设置变更即时重评估；`from==to`/格式非法 = 永不生效（安全侧）。手动
`POST /config/limit` 语义升级为「设定基准」——窗口命中时经
`tick_alt_limits` 重评估取备用/基准优胜者。

**BT 会话热改链路**：trait `apply_bt_session(BtSessionPatch)`（默认
Unsupported）→ `BtEngine` 会话快照（BtSessionCfg）合并后按组全量重放
（discovery / transport / conn 任一组失败快照保持旧值，重试幂等）；内核新增
`lt_apply_conn(port, max_connections)`——`listen_interfaces =
"0.0.0.0:<p>,[::]:<p>"`（apply_settings 自动 re-listen）+ `connections_limit`
（0=不下发）；lt.h 契约注释 / bindings.rs / ffi.rs `Session::apply_conn` /
engine.rs `BtCore::apply_conn` 四层同步；启动期经 `BtEngine::apply_startup_conn`
（serve 装配从 `[bt] listen_port / max_connections`）。

**事件**：`SchedulerEvent::SettingsChanged { keys }`（type_label=
`settings_changed`，known_event_type_labels 锁定 13 变体）。

**验证**：`tests/settings_api.rs` 6 例（快照形状/持久化 round-trip+事件/
备用窗口立即切换/非法 400 零副作用落盘未动/多域应用+restart_required/
代理与 dest_root）+ 窗口判定单测 4 例；daemon 默认 297/0 · bt
--all-targets 359/0 · btcore 37/0 · httpdl 175/0 · 双 clippy 0。

### 2. 前端入仓 + 内嵌 UI + 桌面端（feat 提交 67daeff / 3cf0bae）

- `ui/`：Next.js 15 `output: export` 静态导出；qoder-ui vendored
  （`public/qoder-ui/`）；主题机制对齐官方——`<html data-theme>` 属性
  （forest/bee/mint/parchment 各 light/dark），**修复旧版 class 切换导致的
  主题预览偏差**；视图：任务（过滤/搜索/新建/暂停恢复删除/事件速率）/
  统计（KPI 卡 + 引擎/状态分布 + Catmull-Rom 速率曲线 + SMIL 呼吸端点）/
  日志（事件流 + 类型过滤）/ 设置（八组 + 生效徽标 + 保存 toast）/ 详情抽屉
  （限速/顺序/代理/子文件）。
- `daemon --ui-dir`：tower-http ServeDir fallback（API 优先级不变），
  同一 daemon 二进制同源服务 API+UI；`router_with_ui` 兼容既有
  `router(state)`（24 个测试文件零改动）。
- `desktop/`：Tauri v2 壳——sidecar daemon（externalBin target-triple）+
  TCP 就绪探测 + 主窗加载 daemon URL（同源零 CORS）+ 托盘（显示/退出回收
  子进程）+ `SMART_DL_DESKTOP_PORT` 覆盖；三平台打包 CI
  （`.github/workflows/desktop.yml`，tag `desktop-v*`）；沙盒无
  webkit2gtk 无法本地编译，正确性由 desktop CI 首跑兜底（daemon/ui 侧
  均已实测）。
- e2e（agent-browser 实测）：添加任务 → Downloading → **Completed**，
  产物 `cmp` 逐字节一致；设置改限速 2048/512 → 保存 toast「已应用 2 项，
  已落盘」→ 配置文件回写校验通过；8 主题截图存证。
- 引擎健壮性修复（e2e 发现）：httpdl 段下载接受 RFC 7233 §2.1 的 200
  全量响应（skip seg.start + 截断写 seg.len，写满弃流）——非 Range 服务器
  （python http.server 等）不再 "all mirrors failed"。

### 32. DASH（MPD）下载支持（C-DASH，2026-09-06）

**背景**：与 HLS 同场景（流媒体点播 `.mpd` 清单无下载器自动化）——HLS 覆盖
m3u8 生态后，DASH（ISO/IEC 23009-1）是另一主流分片清单格式，YouTube 类
CDN 与大量 VOD 站点使用 MPD。

**实现**（MPD static VOD 子集，与 HLS v1 同哲学）：
- **识别与分流**：`HttpEngine::add` 对 URL 路径 `.mpd` 后缀（剥 query/frag
  大小写无关）分流 DASH 路径——不走 Range 探测/分段链；任务级代理/
  headers/限速全链同源；`HttpTask.hls: bool` 升级为 `StreamKind` 枚举
  （Plain/Hls/Dash），spawn 循环泛化为 `spawn_stream_loop`（按 kind 分发
  `download_hls`/`download_dash`，abort 约定串双白名单）。
- **MPD 解析**（`dash.rs`，quick-xml 0.37 事件 → 轻量元素树后结构解析，
  metalink 同款依赖）：`type="dynamic"`（live）/多 Period/xlink 外链 →
  拒绝；**选流** = 视频优先（contentType/mimeType video/）→ 集合内最高
  BANDWIDTH Representation，纯音频内容回退最高码率音频轨；DRM
  （ContentProtection）→ 拒绝。
- **分段定址**：SegmentTemplate `duration` 属性 → $Number$ 定址（段数 =
  ceil(Period 时长 × timescale / duration)，时长取 Period@duration 或
  MPD@mediaPresentationDuration，ISO 8601 解析含 Y/M/W 固定换算）；
  `SegmentTimeline` → $Time$ 定址（`<S t d r>` 展开，t 缺省继承前段终点，
  负 r 拒绝）；SegmentList（显式 SegmentURL）；无分段且 Representation
  自带 BaseURL → 单文件整下。模板替换 `$RepresentationID$`/`$Bandwidth$`/
  `$Number$`/`$Time$`（`%0Nd` 零宽、`$$` 转义）；`$SubNumber$`/未知标识符
  → 拒绝。URL 按 BaseURL 链（MPD→Period→AS→Rep）逐级相对解析 +
  `.`/`..` 段压平；SegmentList 对所属元素 base 解析（spec 5.3.9.4.2）。
  SegmentBase（单文件索引 byte-range）/ mediaRange / index → 明确拒绝。
- **顺序下载 + 段账本续传**：init 段（如有）+ 媒体段顺序 append `.part`
  （fMP4 init+media 裸拼接 = 可播放流文件）；`.part.dash-ledger` JSON
  记录清单指纹（sha256 MPD 文本+URL）/已完成段数/字节数（init 计第 0 项）；
  恢复 = 重拉 MPD → 指纹对账（失配作废重下）→ 断点续传，已完成段零重复
  请求（e2e 计数断言）。
- **pause/resume**：abort flag 段间检查点（`dash-aborted` 约定错误静默
  退出）；resume epoch+1 重 spawn 凭账本续传——HLS 同模式。
- **落盘名**：显式名权威；派生 = 入口 URL 清单名去 `.mpd` + `.mp4`；
  total = 0（未知长度语义），done 按字节累计。
- 依赖：quick-xml（workspace 统一 0.37，metalink 同款，无 feature 门）。

**验证**：解析单测 17（duration/Timeline/List 定址、BaseURL 链 + `..`
压平、模板边角 `$$`/未闭合/非法宽度/SubNumber、audio 回退、单文件表示、
ISO 8601、dynamic/多 Period/DRM/SegmentBase/xlink 拒绝、识别与落盘名）
+ e2e 4（duration 定址 init+4 段逐字节 + 名派生 manifest.mp4、timeline
账本续传零重复拉取、dynamic 任务 Error、pause→resume roundtrip）。
门禁：fmt · clippy ×6（workspace ftp/sftp + daemon ftp/nas/ftp,nas/sftp，
-D warnings）全 0 · core 262/0 · httpdl(ftp) 224/0 · httpdl(sftp) 203/0 ·
provider 162/0 · daemon default 304/0 · ftp,nas 316/0 · sftp 309/0。

## 桌面版 BT 引擎装配（S1-d，2026-09-06 批次，PR #90）

### 三平台原生 libtorrent 打包矩阵（desktop workflow）

sidecar（smart-dl-daemon）从 no-BT profile 升级 **`--features bt,ftp,sftp`
全引擎**；libtorrent 以动态库链接，运行期闭包随包分发，用户机器零额外安装。

**布局实证先行**：解剖 desktop-v0.1.0 三平台产物（dpkg-deb -x /
--appimage-extract / .app tar.gz）取得 tauri v2 打包真相——sidecar 安装名
剥 `-<triple>` 后缀（`/usr/bin/smart-dl-daemon`、`Contents/MacOS/…`）、资源根
`/usr/lib/Smart Downloader/`（deb）与 `AppDir/usr/lib/Smart Downloader/`
（AppImage，同相对层）、`Contents/Resources/`（macOS）、安装根 = exe 目录
（Windows）。资源覆盖层 `tauri.{linux,macos,windows}.conf.json` 按此映射。

| 平台 | libtorrent | FFI 内核 | 运行期分发 | 自检 |
|---|---|---|---|---|
| Linux | apt libtorrent-rasterbar-dev（jammy 2.0.8，`#if LIBTORRENT_VERSION_NUM >= 20100` 源内守卫兼容） | bt-linux-setup.sh 同源（g++ -std=c++17 -fPIC → liblt_kernel.a；fakevcpkg `.lib` 别名 + `lib*.so` 符号链接；g++ 驱动链接器 wrapper） | `ldd` 全闭包拷贝（排 glibc 基座，**含 libstdc++**）→ `patchelf` 注入 RUNPATH `$ORIGIN/../lib/Smart Downloader/native/linux`（deb/AppImage 同构；每个 .so 另注 `$ORIGIN`） | 临时安装布局模拟树 + **空 LD_LIBRARY_PATH** ldd 全解析 |
| macOS (aarch64) | brew libtorrent-rasterbar | clang++ -std=c++17（libc++ ABI） | `otool -L` 递归闭包拷贝 → sidecar 与各 dylib `install_name_tool -change` → `@executable_path/../Resources/native/macos/lib/<basename>` → 全量 `codesign -f -s -` 重签 | otool 无残留 brew 绝对引用 |
| Windows (msvc) | vcpkg libtorrent（x64-windows 动态 triplet，build.rs vcpkg 契约原生同构；二进制缓存 `.vcpkg-cache`） | MSVC cl /std:c++17 /MD（vcvars 供 SDK INCLUDE；临时 .cmd 批处理规避 cmd /c 引号剥层） | vcpkg `bin/*.dll` 全量 + CRT 三件套（msvcp140/vcruntime140/vcruntime140_1）→ 资源映射安装根（= exe 目录，DLL 搜索路径首位） | 产物存在性断言 |

**新文件**：`scripts/ci/desktop-bt-linux.sh` / `desktop-bt-macos.sh` /
`desktop-bt-windows.ps1`（setup = native 环境 + GITHUB_ENV；stage = 闭包
分发 + 改写 + 自检）；`tauri.{linux,macos,windows}.conf.json`。

**踩坑记录**：
- `ldd` 行格式为「lib名 => 路径 (地址)」三字段，两字段 read 会把 `=>`
  当路径（闭包 0 个）；
- `cp -r src dst` 当 dst 不存在时 dst 本身成为 src 副本（.so 上移一层，
  RUNPATH 差一级失配）——模拟树必须先 mkdir 再 `cp -r src dst/`；
- `$ORIGIN` 与含空格路径（`Smart Downloader`）经 `LD_DEBUG=libs` 实证
  glibc 展开正常；
- macOS fakevcpkg 必须带 dylib 实名符号链接（ld64 不搜索 brew lib 默认
  路径）+ `c++.lib` 别名（rustc cc 驱动不自动带 C++ 运行时）；
- Linux 侧同理由 `stdc++.lib` 别名进链接行（`-nodefaultlibs` 语义）。

**验证**：本地 Linux 全链——release 构建（5m28s）→ stage 闭包 6 .so
（libtorrent-rasterbar/ssl/crypto/stdc++/z/zstd，trixie 2.0.11 无 boost
实体库依赖）→ 安装布局模拟树 `env -i` 实跑：`serve` /health 200 + magnet
任务 `engine=bt` 落位 + FTP/SFTP 引擎启用日志（RUNPATH 独立解析，无环境
依赖，即 deb/AppImage 安装后运行形态）。macOS/Windows 路径由 desktop
workflow（workflow_dispatch 于 PR 分支）三平台首跑实证。

**版本**：tauri.conf.json / src-tauri Cargo.toml 0.1.0 → **0.2.0**
（desktop-v0.2.0 Release 预留）；Release 文案更新（全引擎 + 原生库随包）。
`desktop/src-tauri/native/` 与 `.vcpkg-cache/` 入 gitignore（CI stage 产物
不入库）。

## 33. RSS 订阅自动下载 + BT 任务 add 后 resume 补链（2026-09-06，PR #91）

### fix(bt)：首次 add 后 handle 永久 paused（磁链实测暴露）

- 内核统一语义（Bug A 修复）：`lt_add_magnet` / `lt_add_torrent_file` /
  `lt_add_torrent_resume` 三入口均 `paused + 非 auto_managed` 落库。恢复重放
  路径（`restore_from`）已有对称 resume（"非 paused 且 BT → engine.resume"），
  但**首次 add 路径缺失**——任务层 add 只调引擎 add 不 resume，magnet 元数据
  抓取与 .torrent 下载从不启动。
- 暴露方式：本地 x.pe 磁链闭环（seed_main 做种 2MB 确定性文件 → daemon
  `magnet:?xt=urn:btih:<ih>&dn=...&x.pe=127.0.0.1:<port>`）——修复前 seeder
  `peers=0` 60s 零连接；补 `BtEngine::add` 末尾 resume 后：元数据（BEP-9）
  → 下载完成 → 终态 Seeding → `cmp` 逐字节一致。
- 新建任务无用户暂停意图，resume 与 P4 G5（重启保持暂停）不冲突；后续用户
  pause 走 pause API（intent 标志位在 alert 循环持续压制复活）。
- 环境注记：沙盒 UDP 出站被封（DHT ping `router.bittorrent.com:6881` 超时
  实测）——公网磁链的 DHT/tracker 发现层依赖真实网络（G1 既有口径）；x.pe
  直连绕过发现层，其余全链真实。

### feat(rss)：RSS 订阅自动下载（qBittorrent RSS 对标，v1）

**解析**（`crates/daemon/src/rss.rs`，quick-xml 0.37，对齐 metalink.rs 哲学）：
- RSS 2.0（`<item>`）与 Atom（`<entry>`）同一解析循环，按 local name 匹配
  （命名空间无关）。
- 条目 URL 三级兜底：RSS 2.0 `<link>` 文本 → Atom `<link href>`（rel=alternate
  优先，无 rel 次之，enclosure 等兜底）→ http(s) 形态 guid。均缺 → 跳过该条
  （逐条容错）；整体无有效条目 → Err（上层 400）。
- Atom 条目标识 `<id>` 与 RSS 2.0 `<guid>` 同映射 guid 字段；缺省 = url；
  同 feed 内 guid 重复保留首见。
- channel/feed 级 `<title>` → 订阅标题（首次拉取后锁定，站点改标题不改写）。

**规则引擎**（无 regex 依赖）：
- `must_contain` 全部关键词（大小写不敏感子串）命中标题才匹配；
  `must_not_contain` 任一命中即排除。
- `feed_id`：Some = 规则只作用于该订阅。
- 命中 → `add_link_task_opts(url, rule.dest, name=条目标题)`（V3 显式名语义）
  + `set_task_tags`（tags 非空时）→ 既有 HTTP 全链（探测/分段/限速/校验/
  事件/Webhook）。
- `item.task_id` 落位 = 去重标记：重复 refresh 不重建；新建规则可回溯未处理
  历史条目。

**API（八端点）**：
- `POST /rss/feeds`（添加即拉取；拉取/解析失败 400 fail-closed 不入库；
  同 URL 409）→ 201 `{id, title, item_count}`
- `GET /rss/feeds`（含 item_count / pending_count）/ `DELETE /rss/feeds/:id`
- `GET /rss/items?feed_id=`（条目 + task_id 透出）
- `POST /rss/rules`（规则名校验 / 关键词至少一组 / feed_id 存在性校验）
  / `GET /rss/rules` / `DELETE /rss/rules/:id`
- `POST /rss/refresh`：全量刷新 → 条目合并（guid 去重）→ 规则匹配 → 自动
  建任务；单 feed 拉取失败降级 `errors` 列表不拖垮整体。

**配置与持久化**：
- `[rss]`：`auto_refresh`（默认 true）/ `refresh_interval_secs`（默认 900，
  ticker 下限 60s 防误配风暴）/ `max_processed_items_per_feed`（默认 200；
  仅截已处理条目——未处理截掉会失去重标记导致重复建任务）。
- rss.json（tasks.json 同目录，`with_storage` 派生）：feeds/items/rules 全量
  持久化，重启恢复。
- serve 刷新 ticker：`auto_refresh=true` 且间隔 >0 时 `rss_refresh_all` 周期
  执行。

**测试**：解析单测 6（RSS2.0 基础/Atom 基础/guid permalink 兜底去重/坏 XML
三态/Atom 自闭合 link 三变体/规则匹配大小写）+ e2e 2（订阅 → 规则 → 自动
建任务 → 真实下载 Completed → 落盘 cmp 逐字节一致 → tag 透传 → 去重 →
rss.json 落盘；Atom 添加 + 规则校验 400 系列 + 坏 feed fail-closed）。

**门禁**：fmt · clippy ×7（daemon 5 变体 + workspace + httpdl ftp，
`-D warnings`）全 0 · daemon bt,ftp,sftp / default / core / httpdl(ftp)
测试全 0 失败。

## 34. qBittorrent 对标补齐批次：添加 peer / 全局 tracker 追加 / 分享率上限 / 超级种子（2026-09-06，Task 38）

> 用户指令：「磁链下载测试 + 设置功能确认 + 对标比特彗星/qBittorrent 的通用下载能力不全就补」。
> 磁链端到端实测先行（暴露缺口），随后按差距清单四项落地。

### 磁链端到端实测（先行，含环境重建）
- 沙盒重置后全量重建：rustup stable 1.98 + `scripts/ci/bt-linux-setup.sh --no-root`
  重建本地 libtorrent 2.0.11 native 资产（bt-native；env.sh 快照）。
- btcore 层：`m0_magnet_e2e`（真实磁链 60s 内 progress>0，1.22s 通过）+
  `magnet_metadata`（magnet → .torrent 抓取全链 + API 往返）3/3 绿。
- daemon API 层：本地 seed_main seeder + `x.pe` 直连磁链 → `POST /tasks` →
  metadata 到达 → 2MB 下载 → **Seeding 态（BT 完成语义）** → `cmp` 逐字节一致。
  实测脚本沉淀 `scripts/magnet-e2e.sh`（seeder ECANCELED 教训：seed-data 目录
  必须先建 + 同端口立即复用会失败——脚本已带随机端口 + 重试）。

### 新能力四项（qbit 对标差距清单落地）
1. **`POST /tasks/:id/peers`（qbit「添加 peer」对标）**：`{addrs:["ip:port"]}`
   逐条 `connect_peer` 注入 BT 任务；部分成功语义（逐条回执 `{addr,ok,error?}`）；
   非法 addr 400 整体拒绝（与 /bt/metadata peers 同口径）、非 BT 任务 409、
   404。引擎链路 ffi `lt_add_peer` → session → BtCore 原本齐全，本次补 API 面。
2. **`bt.extra_trackers`（qbit「自动添加 tracker 到新任务」对标）**：配置 +
   `PUT /settings`（bittorrent 域，整表替换，≤50 条/单条 ≤512）；`add()` 成功后
   逐条 `add_tracker`（best-effort）；快照暴露 `bt_extra_trackers`。
3. **`bt.max_share_ratio`（qbit Share Ratio Limit 对标）**：Seeding 态任务
   `share_ratio ≥ 阈值` → 引擎自动暂停 + `seeding_limit_reached` 事件（执法点 =
   `poll_engine_states` BT 分支；执法逻辑独立 async fn——锁与 await 跨点隔离在
   内部生成器，修 tokio Send 门禁）；0 = 不启用（默认），0..=9999 校验（启动 +
   设置双口径）。快照暴露 `bt_max_share_ratio`。
4. **`POST /tasks/:id/super-seeding`（BitComet 首创/qbit 任务右键对标）**：
   内核新增 `lt_set_seed_mode`（`torrent_flags::seed_mode` set/unset 可逆）→
   bindings → session `set_seed_mode` → BtCore `set_super_seeding` → daemon
   端点（`{enabled:bool}`；做种态生效、下载中设置无效果——与 qbit 语义一致）。

### 基建与测试
- `BtSessionPatch` 增 `extra_trackers`/`max_share_ratio`（核心 trait 补丁面）；
  `BtEngine::new` 增两参（13 处调用点全适配）；`DownloadEngine` trait 增
  `set_super_seeding`（默认 Unsupported）+ `seeding_ratio_limit`（默认 None）。
- 测试 +4：bt_api 3（peers 往返/HTTP 任务双 409/超级种子往返）+ settings_api 1
  （extra_trackers + ratio 应用/快照回读/负值·超限·空条目 400/0 关闭合法）。
- 运行时实测 `scripts/qbit-features-e2e.sh`：settings 快照新键 → extra_trackers
  注入任务 tracker 表 → peers 回执 → super-seeding on/off → settings 热改 →
  磁链下载回归 cmp 一致，全 PASS。

**门禁**：fmt · clippy（workspace excl btcore + btcore + httpdl ftp，
`--all-targets -D warnings`）全 0 · core+btcore+httpdl(ftp) 524 / daemon
default 313 / daemon(bt,nas) --all-targets 384 —— 共 1221 测试 0 失败。

## 35. 做种时长上限（qbit「做种时间限制」对标）+ UI 设置面板同步（2026-09-06，Task 39）

### feat(bt)：`bt.max_seeding_time_min`（做种时长上限）
- **计时登记**：`TaskRecord.seeding_since: Option<Instant>`（仅内存）——BT 任务
  State/Finished 迁移至 Seeding 时登记（apply_bt_alert）；离开 Seeding（手动暂停/
  失败/达标暂停）即清空。重启口径 = 重新 checking 再 Finished 时重新登记（时长
  计本次运行内）。
- **执法升级**：`enforce_seeding_limit` 扩展为 share_ratio + seeding_time 双限制；
  达标 → 复用完整 `pause()` 语义（引擎暂停 + 记录同步 Paused + autosave + 状态
  广播——与手动暂停同口径，此前 F3 只停引擎不同步记录的缺口一并修复）+
  `seeding_limit_reached` 事件（原因明细：ratio/time/双达）。`cache_bt_poll` 返回
  `seeding_since` 供执法入参。
- **设置面**：`[bt] max_seeding_time_min`（分钟，0=不启用，0..=999999 双口径校验）
  + `PUT /settings` bittorrent 域 + 快照 `bt_max_seeding_time_min`；
  `BtSessionPatch.max_seeding_time_min` 热改链路；trait 增
  `seeding_time_limit()`（默认 None，BtEngine 读会话快照）。

### feat(ui)：设置面板补三项（与 Task 38 后端对齐）
- bittorrent 组新增：做种分享率上限 / 做种时长上限（分钟）数字输入 +
  「新任务自动追加 tracker」多行 textarea（每行一条，trim 过滤空行）；
  类型定义 `Settings.bittorrent` 同步扩展。

### 测试
- `state_tests/seeding_limit_tests.rs` 新 7 例（feature bt 门控）：ratio 达标暂停
  （走完整 pause 语义断言：引擎暂停 + Paused + 计时清 + 双事件）/ ratio 未达 /
  时长达标（ Instant 回拨 31min）/ 时长未达 / 双限未启 / 无计时跳过时长限
  （重启恢复口径）/ 非 Seeding 态不执法。FakeEngine 增 `seeding_ratio_limit`/
  `seeding_time_limit` 可编程面。
- settings_api e2e 例扩展 `max_seeding_time_min`（应用 + 快照回读 + 超限 400）。
- UI lint/build 绿；磁链 e2e 回归 PASS（脚本 peer 注入步骤改走真实
  `/tasks/:id/peers` 端点）。

**门禁**：fmt · clippy（workspace excl btcore + btcore，`--all-targets -D warnings`）
全 0 · core+btcore+httpdl(ftp) 524 / daemon default 313 / daemon(bt,nas)
--all-targets 391 —— 全 0 失败。
