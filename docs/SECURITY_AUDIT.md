# 安全审计报告 — smart-downloader 全仓（2026-09-08）

> 审计范围：`crates/{core,httpdl,daemon,btcore,provider,xunlei-*}`、`ffi/`、`desktop/`、`ui/`、脚本与配置。
> 方法：双路并行代码审计（API/凭据面 + 文件系统/传输面）+ 依赖版本核查 + 修复后行为级实弹验证。
> 结论：**发现 4×P1、6×P2、若干 P3；P1 全部修复，P2 修复 5 项、1 项记为接受风险，P3 修复 4 项。**
> 修复批次：batch8（与 batch7 同入 0.2.2 能力批次，PR #112）。

## 一、P1（已全部修复）

### P1-1 空 token 击穿 fail-closed（已修）
- 现场：`serve.rs resolve_http_token` 只过滤 env 空串，config `http_token = ""` 产出 `Some("")` → fail-closed 视为"已配置"放行非回环启动 → `with_http_token` 再把空串过滤成 None → **鉴权全放行 + 网络可达**。
- 修复：config 空串同口径过滤（`serve.rs`）；回归测试锚定 `resolve_http_token(None, Some("")) == (None, false)`。
- 实弹：`http_token = ""` + `addr = "0.0.0.0"` → 启动即拒（exit≠0，报"非回环监听地址但未配置"）。

### P1-2 DNS rebinding → tokenless 回环 RCE 链（已修）
- 现场：无任何 Host 校验。tokenless 模式（默认 `127.0.0.1:8787`，含桌面形态）下，攻击页将 evil.com rebind 到 127.0.0.1 后同源直访 API → 可 `PUT /settings` 热改 `post_download.hook`（任意程序执行）/改 `dest_root` 任意写 → 浏览器 drive-by → RCE。
- 修复：`auth_mw` 顶部新增 **Host/Origin 同源守卫**（`request_from_local_origin`，http.rs）——tokenless 模式下 Host 必须为回环名（或 `[server] extra_allowed_hosts` 白名单）；带 `Origin` 头的跨站请求（CSWSH/简单表单 CSRF）同口径拒绝；HTTP/1.0 无 Host 放行（CLI/健康探针兼容，浏览器链不可绕过）。配置 token 后守卫自动关闭（攻击者无凭据，反代域名不受影响）。
- 实弹：`Host: evil.com` → 403；`Origin: http://evil.com` → 403；`Host: 127.0.0.1` → 200；HTTP/1.0 → 200。

### P1-3 FTP PASV 数据面 SSRF（已修）
- 现场：`parse_pasv` 原样信任服务器应答，数据连接直连任意 `host:port`——恶意/被入侵 FTP 服务器可把客户端导向内网第三方并写入攻击者可控字节。
- 修复：`validate_pasv_target`（ftp.rs）——数据地址 == 控制连接对端 → 放行；指向 loopback/私网（RFC 1918）/链路本地（fe80::/10）/唯一本地（fc00::/7）/v4-mapped 内网 → 拒绝；双公网异主机 → 放行（NAT 场景，curl `--ftp-skip-pasv-ip` 同款取舍）。RETR 与 LIST 两处数据连接全覆盖；`FtpSession` 记录控制对端 IP。
- 回归：`validate_pasv_target_blocks_internal_redirect` 9 断言（含 LAN 内横向与 IPv6）。

### P1-4 SFTP host key 全接受（已修）
- 现场：`AcceptHostKey` 恒 `Ok(true)`——首次连接 MITM 可截获密码（发给伪服务器）与全部数据。
- 修复：**TOFU**（Trust On First Use）——`SftpKnownHosts`（指纹 sha256）未知主机首录 + 接受（持久化 `sftp_known_hosts.json`，0600 唯一 tmp 原子写，与 tasks.json 同目录）；已录匹配接受；**已录失配拒绝**（error 日志指明恢复方式）。serve 注入数据目录路径；缺省内存 TOFU（进程生命周期）。
- 回归：`sftp_known_hosts_tofu_lifecycle`（首录/复连/换钥拒/跨实例回读/0600/内存模式）。

## 二、P2（5 修复 + 1 接受风险）

### P2-1 `GET /settings` 回显代理凭据（已修）
- 现场：`/config` 快照刻意隐藏 proxy（user:pass@），`/settings` 却原样返回——口径不一致，凭据出 API 面。
- 修复：`settings.rs` 快照对 proxy 过 `redact_url`（`***@host`）。PUT 路径使用独立类型化请求体，不受快照脱敏影响（已核实）。

### P2-2 WS/SSE 跨站劫持 CSWSH（已修）
- 并入 P1-2 守卫：带 Origin 的 WS 升级/事件流请求在 tokenless 模式同口径 403；配置 token 后 WS 需 Authorization 头（浏览器不可带，安全方向正确）。

### P2-3 server 层零超时零并发（已修并发）
- 修复：全局 `ConcurrencyLimitLayer(256)`（tower）——slowloris/海量空闲 SSE-WS 会话资源耗尽面收敛（qbit WebUI 同量级）。
- 残留：header-read timeout 需 hyper_util 层配置（axum 0.7 `serve` 未暴露），记入遗留建议。

### P2-4 provider 云端文件名未净化即 join（已修）
- 现场：`FallbackSink::transfer` 用 provider 原样透传的文件名 `dest_root.join(rel).parent() + create_dir_all`——绝对路径名可整体替换 base 在 dest_root 外建目录。
- 修复：`sanitize_rel` 先行，非法即 `SinkError::Failed`。

### P2-5 ftp:// 明文凭证无告警（已修）
- 修复：`add_ftp_task_opts` 显式 `warn`（凭据与数据均明文；建议 ftps://——其 RFC 4217 全链强制 TLS + webpki-roots 严格校验无 insecure 口子，审计确认）。

### P2-6 自定义头跨 host 重定向携带（接受风险，未修）
- 现场：任务自定义头（Cookie 等）在引擎手动/reqwest 重定向下不会被剥离（reqwest 仅剥库内标准三头）。恶意源 302 可收割任务凭据头。
- 处置：**接受风险**——修复需引擎层手动重定向循环（改动大）；缓释因素：cookie jar 域隔离正确、默认 client 仅库内标准头、任务头多为用户自配私有源凭据（源站本身受信任域）。列入遗留建议首位。

## 三、P3（4 修复 + 其余记录）

### 已修
1. `is_loopback_addr` 纯字符串前缀判定漏 `[::ffff:127.0.0.1]` → 改 SocketAddr/IpAddr 语义判定（v4-mapped 归一）+ 回归。
2. `.part.progress` 账本固定 tmp 名 + 默认权限（多 worker 并发互踩 + 世界可读）→ 唯一 tmp（纳秒）+ 0600，对齐 tasks.json 配方；fastresume `<ih>.fastresume.tmp` 同修。
3. hook 子进程继承全量环境（含 `SMART_DL_HTTP_TOKEN`/`SD_L1_TOKEN`/`SD_NAS_*`）→ `env_clear` + 白名单重建（剥离敏感项，SD_TASK_* 上下文保留）。
4. `sanitize_rel` 不拒 Windows 保留设备名（CON/PRN/AUX/NUL/COM1-9/LPT1-9 含 `CON.txt` 形式）与尾点/尾空格 → 末段校验补齐（中间目录段不误伤）+ 回归。

### 记录（不修）
- SSE `?token=` 进 URL（EventSource 规范限制的功能性回退，仅精确路径；UI localStorage 存 token 同理——XSS 场景可读）。
- auto token 打印 stdout（桌面壳读取契约）。
- webhook URL 无 host 校验（操作员配置语义 = 信任域内 SSRF；payload 无凭据；浏览器链已被 P1-2 守卫闭合）。
- BT magnet `tr=`/webseed/tracker 任意主机（BT 客户端功能语义，引擎信任边界内）。
- HLS/DASH 清单驱动的 key/段 URL 任意 http(s) 拉取（下载器功能语义；GET-only 无回显通道）。
- BT 文件落盘依赖 libtorrent 内部清洗（纵深：Rust 解析层已拒 `..`/绝对路径）。
- 内部路径进错误响应（鉴权后可见）。
- 迅雷 provider 冒用官方客户端身份（client_id/盐，无 secret；ToS/法律域）。

## 四、正面确认（审计通过项）

- **鉴权覆盖**：单一 auth_mw 层覆盖全部路由 + 两个 fallback（axum 0.7.9 layer 语义逐条核实）——`/metrics`、`/health`、静态 UI、NAS 反代均无豁免；token 恒时比较（长度泄露注释自洽）。
- **loopback 放行**：启动期一次性判定，无 XFF/远端 IP 逐请求逻辑（伪造面不存在）；非回环 + 无 token 拒绝启动；热重载不改 addr/token。
- **路径穿越主干**：`sanitize_rel` 唯一裁决点 × 14 落盘调用点全覆盖；`allowed_roots` 白名单 6 条 add 路径全过；symlink 逃逸逐级检查；CD 派生名链逐级 sanitize。
- **凭据卫生**：日志宏全仓扫描 0 敏感值打印；`redacted_debug()` 剥 userinfo/敏感 query/头（含侧信道搜索语料）；tasks.json/bans.json/rss.json/config/token 文件全 0600 唯一 tmp 原子写。
- **反序列化 DoS**：axum 默认 2MB body 限制全局在（未 disable）；serde_json 128 层递归默认；/events、batch、limit 均有界。
- **TLS 面纯净**：Cargo.lock 内 native-tls/openssl 零命中（reqwest rustls-tls + webpki-roots）；全仓无 `dangerously_accept_invalid*`。
- **无 zip/解压面**（zip slip 不适用）；NAS 反代目标固定（env 注入非开放代理）。
- **依赖版本**（Cargo.lock 在库）：reqwest 0.12.28 / rustls 0.23.43 / tokio 1.53.1 / quick-xml 0.37.5 / russh 0.63.2 / aes+cbc（RustCrypto）——凭知识核查无已知高危版本；libtorrent 为系统级依赖（版本随发行版）。

## 五、验证与证据

- 门禁：fmt 绿；clippy 全口径绿（默认 + ftp/nas/sftp + bt --all-targets）；core+provider 426 / httpdl-ftp 231 / httpdl-sftp 205 / btcore 42 / daemon 默认 329 + bt 407 全绿。
- 实弹：acceptance-live.sh 13/13（401 门禁 ×2 / Sintel infohash / 本地磁链 e2e 逐字节 / HTTP 1MB cmp / S2 零残留）——守卫对 127.0.0.1 正常访问零影响。
- 行为级安全验证：见 §一 各"实弹"条目（403×2 / 200×2 / 拒启动 ×1）。

## 六、遗留建议（下一批）

1. P2-6 自定义头重定向剥离（引擎手动重定向循环）。
2. hyper header-read timeout（等 axum/hyper-util 层暴露或切手动 serve）。
3. SFTP known_hosts 换钥的 UI 呈现（当前为 error 日志 + 人工删文件）。
4. `extra_allowed_hosts` 热重载支持（当前启动注入）。

## 七、工具化安全审计（batch9，2026-09-08）

> 方法升级：人工双路审计（§一–§五）之后，引入三件行业标准审计工具做
> 全量机器扫描，并把结论固化为可复跑的仓库门禁。

### 工具与覆盖面

| 工具 | 版本 | 覆盖面 | 结果 |
|------|------|--------|------|
| cargo-audit | 0.22.2 | Cargo.lock × RustSec advisory DB（1242 条） | 初扫 3 漏洞 → 修复后 **0** |
| cargo-deny | 0.20.2 | advisories / bans / licenses / sources | 初扫多组失败 → 配置后 **全绿** |
| gitleaks | 8.30.1 | git 全历史（317 commits，21.6MB）+ 工作树 | 初扫 36 命中 → 取证豁免后 **双绿** |

### 发现与修复

1. **RustSec 漏洞 3 项（全修）**：均为网络面 DoS 类，与下载器场景强相关——
   - `h2` 0.4.15 unbounded empty DATA frames（RUSTSEC-2026-0258）→ **0.4.19**
     （axum/hyper HTTP/2 服务器与客户端路径）；
   - `quick-xml` 0.37.5 两项（RUSTSEC-2026-0194 二次方复杂度 / 2026-0195
     命名空间声明无界分配，内存耗尽 DoS）→ **0.41.0**（DASH/Metalink4/RSS
     清单解析入口，直接依赖升级）。
2. **quick-xml 0.37→0.41 API 迁移（三解析器全量适配）**：`BytesText::unescape`
   移除、`decode_and_unescape_value` 弃用、**实体引用拆分为独立
   `Event::GeneralRef` 事件**。`dash.rs` / `metalink.rs` / `rss.rs` 补
   GeneralRef 分支（数字字符引用 + 预定义实体解析），metalink 由"逐事件
   覆盖"升级为"累积 + End 落位"（等价语义超集）；新增实体切分回归测试
   `unescapes_numeric_char_refs_and_fragmented_text` 钉死行为。
3. **自身 crate 许可元数据缺失**：5 个主 crate 无 `license` 字段（cargo-deny
   licenses 初扫全拒）→ workspace 统一 `license = "MIT OR Apache-2.0"` +
   `publish = false`（与 xunlei-* 既有声明一致；应用型 workspace 不发布
   crates.io）。仓库根 LICENSE 文件属维护者决策，另行建议。
4. **gitleaks 36 处历史命中分类取证**：
   - 31 处为**厂商公开常量**（迅雷客户端 device/签名常量 28 字节 ×25、
     NAS 设备流 client 常量 22 字节 ×5、厂商前端 bundle 内嵌 key ×2 中的
     公开部分）——public-by-construction，非用户凭证；
   - `login_page.rs` 2 处为 **alg=none 测试样例 JWT**（fixture，非真实 token）；
   - 1 处为历史取证产物中的**会话级 JWT**（HEAD 已删、早已过期）——记录
     在案并建议吊销对应 NAS 会话；不做公开仓库历史重写（破坏性操作）；
   - 现行代码核实：`scripts/nas/*.py` 已改 `SD_XL_CLIENT_ID/SECRET`
     环境变量注入（修复在前，命中仅存历史版本）。
5. **构建产物误报**：`target/` 下加密库 `.rmeta` 元数据的 private-key 命中
   22 处——路径豁免。

### 门禁固化（新增文件）

- `deny.toml`：advisories 全阻断 + yanked=deny；外部通配符版本 deny
  （workspace 内 path 依赖经 `publish=false` 合规放行）；license 白名单
  （MIT/Apache-2.0/BSD/ISC/Unicode-3.0/Zlib/MPL-2.0/CC0/BSL-1.0 等）+
  ring 许可澄清；sources 锁定 crates.io。
- `.gitleaks.toml`：默认规则 + 精准 allowlist（逐条理由：两条厂商公开
  常量正则、alg=none fixture 前缀、取证产物与构建产物路径）——不做无理由
  目录级宽免，未来真实泄漏仍会浮出。
- `.github/workflows/ci.yml`：新增 **security job**（cargo-deny-action
  四组检查 + gitleaks-action 全历史扫描），与 rust/bt-integration 并列。

### 验证终态

- cargo-audit 复扫 **0 漏洞**；cargo-deny 四组 **全绿**；gitleaks 历史 +
  工作树 **双绿**。
- 门禁回归：fmt 绿；clippy（ftp / sftp 口径，-D warnings）绿；core 264 /
  provider 162 / httpdl-ftp 全绿 / httpdl-sftp 205 / daemon 默认 **330**（+1
  实体切分回归）/ daemon ftp+nas / sftp 全绿 / btcore / daemon bt 全绿。
