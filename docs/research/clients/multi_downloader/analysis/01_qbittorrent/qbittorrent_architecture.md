# qBittorrent 源码架构深度分析

> 适用于：基于 qBittorrent master 分支（libtorrent 2.x 系）+ libtorrent `RC_2_0` 分支
> 分析对象：BT 协议内核实现、libtorrent 包装层、策略/算法层
> 目标读者：基于 Rust 实现多协议下载器的工程师
> 源码路径：`/home/z/my-project/repos/qbittorrent/`、`/home/z/my-project/repos/libtorrent/`

---

## 1. 概览

qBittorrent 是 Linux/Windows/macOS 桌面端最广泛使用的 BitTorrent 客户端，没有之一。它的特殊之处不在于"自己重新发明 BT 协议"，而在于把 **libtorrent（Arvid Norberg 维护的 C++ 引擎）** 当作内核，在其之上构建了一层完整的进程管理、配置持久化、Torrent 元信息恢复、Tracker 嵌入、Web UI、RSS 自动下载、搜索引擎（Python）、Lua 插件系统、IP 过滤等工程化设施。换句话说，qBittorrent 本质上是"libtorrent 之上的应用壳"，但壳的复杂度足以承载 BT 协议之外的所有运营、运维、可观测性能力。

从 Rust 多协议下载器设计的视角，qBittorrent 的价值在于：

1. 它是 **libtorrent 设置面（`settings_pack`）的最权威使用范例**——几乎所有 libtorrent 文档里语焉不详的开关在 qBittorrent 里都有对应的 GUI/CLI 入口与默认值。
2. 它演示了如何用 **Qt event loop + libtorrent 的 alert callback** 把"异步、单线程、回调式"的 libtorrent 内核适配到"信号/槽、对象树"的 Qt 模型上——这是用 Rust `tokio`/`async` 适配 BT 内核时必须直面的同构问题。
3. 它把 **Torrent 持久化（BencodeResumeDataStorage vs DBResumeDataStorage）**、**断电恢复**、**升级迁移**、**IP 过滤热加载**、**带宽调度器**这些工程问题完整地走过一遍，包含了大量踩坑后的细节（如双文件原子写、fastresume 被拒后自动暂停）。

本文档自顶向下分层剖析：进程/线程模型 → 启动/配置 → SessionImpl → TorrentImpl → libtorrent 内核（peer_connection / bandwidth / choker / piece_picker / disk_io / utp）→ 三大核心算法（peer 评分、带宽分配、连接生命周期）→ 对 Rust 实现的启示。

---

## 2. 架构总览

### 2.1 顶层模块划分

qBittorrent 源码在 `src/` 下显式分为四个独立编译单元，由 CMake 的 `DISABLE_GUI`、`DISABLE_WEBUI` 宏控制是否参与编译：

```
src/
├── app/              // 程序入口、Application 单例、命令行参数、单实例、信号处理、文件日志
├── base/             // 核心库（与 GUI 无关），headless 也要用
│   ├── bittorrent/   // libtorrent 包装层：SessionImpl / TorrentImpl / PeerInfo / Tracker / ResumeDataStorage …
│   ├── net/          // 下载管理器、GeoIP、SMTP、DNS 更新、PortForwarder 抽象
│   ├── http/         // 内嵌 HTTP 服务（WebUI 与内置 tracker 复用）
│   ├── rss/          // RSS 订阅与自动下载器
│   ├── preferences   // 旧式"Preferences"单例（Qt 仍兼容的 QSettings 风格）
│   ├── settingsstorage // 新式 key-value 存储（QSettings + QVariantHash + 5s 节流落盘）
│   ├── plugins/      // Lua 插件宿主（luabridge）
│   └── 3rdparty/lua  // 内嵌 Lua 5.4
├── gui/              // Qt Widgets GUI（可禁用）
├── webui/            // 内嵌 HTTP/Web UI（基于 base/http）
└── searchengine/     // Python nova3 搜索引擎（独立进程，通过 stdio 通信）
```

`base` 是单进程内的库，所有命名空间均在 `BitTorrent` / `Net` / `RSS` / `Http` 下；`app` 提供唯一的 `Application` 类，根据编译配置派生自 `QApplication`（GUI）或 `QCoreApplication`（headless），并同时实现 `IApplication` / `IGUIApplication` 抽象接口，让 `base` 不直接依赖 `gui`。

### 2.2 进程 / 线程模型

qBittorrent 是**单进程、多线程、Qt 事件循环驱动**的程序。其线程拓扑如下：

```
┌─────────────────────────────────────────────────────────────┐
│                       qBittorrent 进程                       │
│                                                             │
│  ┌────────────────────────────────────────────────────┐    │
│  │ Qt Main Thread (QApplication::exec, event loop)    │    │
│  │ ─ GUI widget / MainWindow / WebUI HTTP handler     │    │
│  │ ─ SessionImpl 信号槽                               │    │
│  │ ─ alert 派发 (readAlerts 槽)                       │    │
│  │ ─ QTimer: refresh / resumeData / seedingLimit /    │    │
│  │   bandwidthScheduler / wakeupCheck / trackerURL   │    │
│  └────────────────────────────────────────────────────┘    │
│           ▲ async (Qt::QueuedConnection)                    │
│           │ set_alert_notify callback                       │
│  ┌────────────────────────────────────────────────────┐    │
│  │ libtorrent internal io_context (network thread)     │    │
│  │ ─ 所有 socket I/O、加密握手、uTP                    │    │
│  │ ─ disk_io_thread_pool (set_max_threads=N)          │    │
│  │ ─ tracker_manager (HTTP/UDP/WebSocket tracker)     │    │
│  │ ─ DHT (kademlia)                                   │    │
│  │ ─ LSD (multicast UDP)                              │    │
│  │ ─ UPnP / NAT-PMP                                   │    │
│  └────────────────────────────────────────────────────┘    │
│                                                             │
│  ┌────────────────────────────────────────────────────┐    │
│  │ SessionImpl::m_ioThread (QThread)                  │    │
│  │ ─ FreeDiskSpaceChecker                             │    │
│  │ ─ FileSearcher（在文件树里查找部分下载文件）       │    │
│  │ ─ TorrentContentRemover（异步删除 Torrent 内容）   │    │
│  └────────────────────────────────────────────────────┘    │
│                                                             │
│  ┌────────────────────────────────────────────────────┐    │
│  │ SessionImpl::m_asyncWorker (QThreadPool, N=1!)      │    │
│  │ ─ 串行执行所有 libtorrent 异步操作（reload、move …）│    │
│  └────────────────────────────────────────────────────┘    │
│                                                             │
│  ┌────────────────────────────────────────────────────┐    │
│  │ BencodeResumeDataStorage::m_ioThread               │    │
│  │ ─ fastresume / .torrent / queue 文件的写盘         │    │
│  └────────────────────────────────────────────────────┘    │
│                                                             │
│  ┌────────────────────────────────────────────────────┐    │
│  │ FilterParserThread (QThread)                       │    │
│  │ ─ 解析 eMule DAT / PeerGuardian P2P/P2B IP 过滤表  │    │
│  └────────────────────────────────────────────────────┘    │
│                                                             │
│  ┌────────────────────────────────────────────────────┐    │
│  │ sessionTerminateThread (临时，仅退出时存在)        │    │
│  │ ─ 析构 lt::session_proxy（可能耗时数秒）            │    │
│  └────────────────────────────────────────────────────┘    │
│                                                             │
│  ┌────────────────────────────────────────────────────┐    │
│  │ searchengine nova3 (Python 子进程)                 │    │
│  │ ─ stdin/stdout JSON RPC                            │    │
│  └────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

**关键设计**：

- **libtorrent 在内部维护自己的 io_context**——这是 libtorrent 的"网络线程"。它并不使用 Qt 的 event loop。qBittorrent 通过 `m_nativeSession->set_alert_notify([this]{ QMetaObject::invokeMethod(this, &SessionImpl::readAlerts, Qt::QueuedConnection); })` 在 `initializeNativeSession()` 里建立**单向桥接**：libtorrent 网络线程产生 alert → 通过 `set_alert_notify` 回调把 `readAlerts` 槽函数排到 Qt 主线程事件队列里执行。所有 alert 处理都在 Qt 主线程完成，避免锁。源码：`sessionimpl.cpp:1848-1851`。
- **`m_asyncWorker` 显式设为单线程**（`setMaxThreadCount(1)`，源码 `sessionimpl.cpp:651`）。注释明确："It is required to perform async access to libtorrent sequentially"。libtorrent 的 `apply_settings`、`async_add_torrent` 等并非线程安全，所以 qBittorrent 用单线程池把所有"需要离开 Qt 线程调用 libtorrent"的代码串行化。
- **析构期独立线程**：`~SessionImpl()` 把 `lt::session_proxy`（一个会等待 libtorrent 内部所有 socket 关闭的对象）扔到独立的 `sessionTerminateThread` 去析构（源码 `sessionimpl.cpp:793-798`），避免阻塞 Qt 主线程导致 UI 卡死。

### 2.3 启动流程：main() → SessionImpl 完整调用链

从 `src/app/main.cpp:179` 的 `int main(int argc, char *argv[])` 进入，到 SessionImpl 完成 resume data 加载并发出 `restored()` 信号，调用链如下：

```
main()                                                  [app/main.cpp:179]
 ├─ adjustLocale() / adjustFileDescriptorLimit()        [Unix: 提高 RLIMIT_NOFILE]
 └─ Application app(argc, argv)                          [app/application.cpp:293]
     ├─ parseCommandLine()
     ├─ Logger::initInstance()
     ├─ Profile::initInstance(profileDir, …)             // 决定配置目录（portable 模式）
     ├─ ApplicationInstanceManager                       // 单实例：QLocalSocket + 锁文件
     ├─ SettingsStorage::initInstance()                  // 读 qBittorrent.ini / .conf
     ├─ Preferences::initInstance()                     // 兼容旧 API
     ├─ upgrade()                                        // 迁移老版本配置
     └─ initializeTranslation()
 └─ app->exec()                                          [application.cpp:887]
     ├─ applyMemoryWorkingSetLimit() / applyMemoryPriority()  // Windows
     ├─ Net::ProxyConfigurationManager::initInstance()
     ├─ Net::DownloadManager::initInstance()             // QNetworkAccessManager 包装
     ├─ BitTorrent::Session::initInstance()              // 构造 SessionImpl
     │   └─ SessionImpl::SessionImpl()                   [sessionimpl.cpp:479]
     │       ├─ 初始化 ~150 个 CachedSettingValue 字段
     │       ├─ m_alerts.reserve(1024)                   // 预分配 alert 数组
     │       ├─ initializeNativeSession()               [sessionimpl.cpp:1781]
     │       │   ├─ loadLTSettings()                    // 构造 settings_pack
     │       │   ├─ new lt::session(sessionParams, paused)  // ⚠ 启动时 paused
     │       │   ├─ set_alert_notify([]{ readAlerts; }) // 桥接 libtorrent → Qt
     │       │   ├─ add_extension(smart_ban_plugin)
     │       │   ├─ add_extension(ut_metadata_plugin)
     │       │   ├─ add_extension(ut_pex_plugin)         // 若 PeXEnabled
     │       │   └─ add_extension(NativeSessionExtension)
     │       ├─ configureComponents()                    // IP filter / peer class
     │       ├─ enableBandwidthScheduler()               // 若启用
     │       ├─ loadCategories() / 加载 tags
     │       ├─ m_ioThread->start()                     // 启动 QThread
     │       ├─ initMetrics()                            // 把 libtorrent metric 名 → index
     │       ├─ loadStatistics()
     │       ├─ new PortForwarderImpl(this)              // UPnP/NAT-PMP
     │       ├─ enableTracker(isTrackerEnabled())        // 内嵌 tracker
     │       └─ prepareStartup()                         [sessionimpl.cpp:1438]
     │           ├─ 实例化 ResumeDataStorage
     │           │   (DBResumeDataStorage 或 BencodeResumeDataStorage，按 ResumeDataStorageType)
     │           ├─ 若 storage 类型与现存文件不符：临时构造 startupStorage 用作迁移
     │           ├─ 连接 startupStorage::loadStarted → handleLoadedResumeData
     │           ├─ 连接 startupStorage::loadFinished → context->isLoadFinished = true
     │           ├─ 连接 addTorrentAlertsReceived → 减少 processingResumeDataCount
     │           └─ startupStorage->loadAll()            // 触发异步加载
     │
     ├─ SessionImpl::handleLoadedResumeData(ctx)         [sessionimpl.cpp:1508]
     │   └─ 循环：保持 processingResumeDataCount < MAX_PROCESSING_RESUMEDATA_COUNT
     │       └─ processNextResumeData(ctx)               [sessionimpl.cpp:1543]
     │           ├─ 读 LoadTorrentParams（ltAddTorrentParams + qBt 字段）
     │           ├─ 恢复 category / tags（必要时 addCategory）
     │           ├─ userdata = new ExtensionData       // 给 NativeTorrentExtension 用
     │           └─ m_nativeSession->async_add_torrent(p)   // 提交给 libtorrent
     │               └─ alert 回来 → createTorrent() → 加入 m_torrents
     │
     └─ 当 processingResumeDataCount==0 且 isLoadFinished →
        endStartup(ctx)                                  [sessionimpl.cpp:1719]
         ├─ 若 storage 类型切换：保存 queue，删除旧 storage 文件
         ├─ 若 !m_isPaused: m_nativeSession->resume()  // ⚠ 全局才真正开始下载
         ├─ enqueueRefresh()                            // 启动 refresh 循环
         ├─ m_resumeDataTimer 启动                      // 周期 saveResumeData
         ├─ wakeupCheckTimer (30s)                       // 检测系统休眠后重 announce
         └─ m_isRestored = true; emit restored();
```

**关键设计点**：

- **libtorrent 在 `paused` 状态构造**（`sessionimpl.cpp:1834/1836`），所有 torrent 通过 `async_add_torrent` 注入但不会开始下载；直到 `endStartup()` 里 `m_nativeSession->resume()` 才整体启动。这避免了启动过程中 resume data 加载顺序与 libtorrent 内部 auto-manage 调度相互竞争。
- **`MAX_PROCESSING_RESUMEDATA_COUNT` 限制并发**——qBittorrent 不一次性把所有 torrent 都 `async_add_torrent`，而是分批。每批完成后通过 `addTorrentAlertsReceived` 信号触发下一批。源码：`sessionimpl.cpp:1486-1503`。
- **`NativeTorrentExtension` 的副作用**：每个 torrent 加载时，构造函数会读 `ExtensionData`（在 `processNextResumeData` 里 `userdata = new ExtensionData` 注入），并且 `on_state` 钩子会在 `downloading_metadata` 或 `checking_files` 状态下自动 `unset_flags(auto_managed); pause();`（源码 `nativetorrentextension.cpp:54-64`）。这是一个相当取巧的"延迟启动"机制——让 libtorrent 先做完 metadata/resume data 检查，再由 qBittorrent 决定是否真的恢复。

---

## 3. 启动与配置流程

### 3.1 配置存储三层

qBittorrent 的配置系统由三层组成：

| 层 | 文件 | 角色 |
|---|---|---|
| L1：原生 QSettings 包装 | `base/settingsstorage.cpp` | 把所有配置存在一个 `QVariantHash m_data` 内存表里，落盘到 `qBittorrent.ini`（Unix）或 `qBittorrent.conf`（Windows）。**原子写**：先写 `qBittorrent_new`，再 rename 替换，避免断电损坏。5 秒 debounce 节流。 |
| L2：模板化访问器 | `base/settingvalue.h` | `SettingValue<T>` 提供 `T get(default)` / `operator=(T)`；`CachedSettingValue<T>` 在内存里缓存当前值，避免每次访问都查 hash。`T` 必须满足 `Stringable`（用户可编辑文本形式）或 `Q_DECLARE_METATYPE`。 |
| L3：领域包装 | `base/preferences.cpp` + `SessionImpl` 成员 | `Preferences` 单例暴露 GUI / WebUI / 邮件 / 命令行 等杂项；`SessionImpl` 把 ~150 个 BT 相关配置作为 `CachedSettingValue` 成员直接持引用。 |

`SettingsStorage` 的核心实现（`settingsstorage.cpp`）：

- **存储结构**：`QVariantHash m_data` + `QReadWriteLock m_lock`。读用 `QReadLocker`，写用 `QWriteLocker`。
- **写入策略**：`storeValueImpl` 设置 `m_dirty=true` 并启动 5 秒单次 `QTimer`（`m_timer.setSingleShot(true); m_timer.setInterval(5s)`）。`save()` 时若 `!m_dirty` 直接返回，避免无谓写盘。
- **崩溃恢复**：`readNativeSettings()` 优先检查 `qBittorrent_new` 文件是否存在；若存在说明上次未正常退出（断电 / 强杀），用 `_new` 替换正式文件（`settingsstorage.cpp:141-163`）。
- **键命名约定**：所有 BT 相关键以 `BitTorrent/Session/` 前缀（通过 `BITTORRENT_SESSION_KEY` 宏），全局键以 `BitTorrent/` 前缀。示例：`BitTorrent/Session/Port`、`BitTorrent/Session/MaxConnections`、`State/BannedIPs`。

`SessionImpl` 构造函数里典型的成员初始化方式（`sessionimpl.cpp:539`）：

```cpp
m_maxConnections(BITTORRENT_SESSION_KEY(u"MaxConnections"_s), 500, lowerLimited(0, -1))
```

意思是：键名 `BitTorrent/Session/MaxConnections`，默认 500，校验函数 `lowerLimited(0, -1)`（小于 0 时强制为 -1 表示无限制）。

### 3.2 libtorrent settings_pack 映射

qBittorrent 的核心职责之一是把自身 100+ 个配置项翻译成 libtorrent 的 `lt::settings_pack`。这个映射在 `SessionImpl::loadLTSettings()` 里完成（`sessionimpl.cpp:1943-2262`，约 320 行）。下面是关键映射表（节选）：

| qBittorrent 配置 | libtorrent settings_pack 字段 | 默认值 | 用途 |
|---|---|---|---|
| `m_isDHTEnabled` | `enable_dht` | true | DHT 开关 |
| `m_isLSDEnabled` | `enable_lsd` | true | 本地服务发现（多播） |
| `m_isPeXEnabled` | （决定是否 `add_extension(ut_pex_plugin)`） | true | Peer Exchange |
| `m_encryption` (0/1/2) | `out_enc_policy`/`in_enc_policy`/`allowed_enc_level`/`prefer_rc4` | 0 (Enabled) | 加密策略：0=启用、1=强制、2=禁用；强制 RC4 |
| `m_maxConnections` | `connections_limit` | 500 | 全局最大连接数 |
| `m_maxUploads` | `unchoke_slots_limit` | 20 | 全局上传槽位 |
| `m_maxConnectionsPerTorrent` | （在 add_torrent_params 里设置） | 100 | 每 torrent 连接数 |
| `m_chokingAlgorithm` | `choking_algorithm` | `fixed_slots_choker` | choking 算法 |
| `m_seedChokingAlgorithm` | `seed_choking_algorithm` | `fastest_upload` | 种子 choking |
| `m_btProtocol` (Both/TCP/UTP) | `enable_incoming_tcp`/`outgoing_tcp`/`incoming_utp`/`outgoing_utp` | Both | 协议开关 |
| `m_utpMixedMode` (TCP/Proportional) | `mixed_mode_algorithm` | `prefer_tcp` | uTP/TCP 混合策略 |
| `m_isUTPRateLimited` | （peer_class_type_filter，见 `configurePeerClasses`） | true | uTP 是否受限 |
| `m_isAnonymousModeEnabled` | `anonymous_mode` | false | 匿名模式（禁统计、改 UA、不 announce 等） |
| `m_diskCacheSize` | `cache_size` (libtorrent 1.x) / 不存在(2.x) | -1 (auto) | 磁盘缓存大小 |
| `m_diskQueueSize` | `max_queued_disk_bytes` | 100MB (lt2) | 磁盘队列字节上限 |
| `m_asyncIOThreads` | `aio_threads` | 10 | 磁盘 IO 线程数 |
| `m_checkingMemUsage` | `checking_mem_usage` | 32 (×64KiB = 2MiB) | 校验内存 |
| `m_peerTurnover`/`Cutoff`/`Interval` | `peer_turnover`/`peer_turnover_cutoff`/`peer_turnover_interval` | 4/90/300 | Peer 轮换策略 |
| `m_isQueueingEnabled` + `m_maxActive*` | `active_downloads`/`active_seeds`/`active_limit` + `dont_count_slow_torrents` | off | 队列系统 |
| `m_announceToAllTrackers`/`Tiers` | `announce_to_all_trackers`/`announce_to_all_tiers` | false/true | Tracker announce 策略 |
| `m_outgoingPortsMin`/`Max` | `outgoing_port`/`num_outgoing_ports` | 0/0 | 出站端口范围 |
| `m_UPnPLeaseDuration` | `upnp_lease_duration`/`natpmp_lease_duration` | 0 | UPnP/NAT-PMP 租期 |
| `m_peerDSCP` | `peer_dscp` | 0x01 | DSCP/TOS 标记 |
| `m_includeOverheadInLimits` | `rate_limit_ip_overhead` | false | 是否把 IP/TCP 头算入限速 |
| `m_ignoreLimitsOnLAN` | （peer class filter，见 `configurePeerClasses`） | false | 局域网不限速 |
| `m_blockPeersOnPrivilegedPorts` | `no_connect_privileged_ports` | false | 拒绝特权端口 peer |
| `m_validateHTTPSTrackerCertificate` | `validate_https_trackers` | true | HTTPS tracker 证书校验 |
| `m_SSRFMitigationEnabled` | `ssrf_mitigation` | true | SSRF 缓解（拒绝内网 tracker） |
| `m_multiConnectionsPerIpEnabled` | `allow_multiple_connections_per_ip` | false | 允许同 IP 多连接 |
| `m_maxConcurrentHTTPAnnounces` | `max_concurrent_http_announces` | 50 | 并发 HTTP tracker announce |
| `m_stopTrackerTimeout` | `stop_tracker_timeout` | 2 (秒) | 停止时 tracker 通知超时 |
| `m_sendBufferWatermark`/`LowWatermark`/`Factor` | 同名 | 500/10/50 | 发送缓冲水位（×1024 字节） |
| `m_connectionSpeed` | `connection_speed` | 30 | 主动连接速度（每秒尝试数） |
| `m_socketBacklogSize` | `listen_queue_size` | 30 | listen backlog |

**关键代码片段（`sessionimpl.cpp:2180-2214`）**——uTP / TCP / 混合模式的开关：

```cpp
switch (btProtocol()) {
case BTProtocol::Both: default:
    settingsPack.set_bool(lt::settings_pack::enable_incoming_tcp, true);
    settingsPack.set_bool(lt::settings_pack::enable_outgoing_tcp, true);
    settingsPack.set_bool(lt::settings_pack::enable_incoming_utp, true);
    settingsPack.set_bool(lt::settings_pack::enable_outgoing_utp, true);
    break;
case BTProtocol::TCP:  // 仅 TCP
    settingsPack.set_bool(lt::settings_pack::enable_incoming_utp, false);
    settingsPack.set_bool(lt::settings_pack::enable_outgoing_utp, false);
    break;
case BTProtocol::UTP:  // 仅 uTP
    settingsPack.set_bool(lt::settings_pack::enable_incoming_tcp, false);
    settingsPack.set_bool(lt::settings_pack::enable_outgoing_tcp, false);
    break;
}
switch (utpMixedMode()) {
case MixedModeAlgorithm::Proportional:
    settingsPack.set_int(lt::settings_pack::mixed_mode_algorithm,
                         lt::settings_pack::peer_proportional); break;
default:
    settingsPack.set_int(lt::settings_pack::mixed_mode_algorithm,
                         lt::settings_pack::prefer_tcp); break;
}
```

### 3.3 延迟配置（deferred configure）

libtorrent 的 `apply_settings()` 是同步的且开销不小（涉及 socket 重建等）。qBittorrent 把所有配置变更通过 `configureDeferred()` 槽合并到主线程下一次事件循环里执行（`sessionimpl.cpp:5873`）：

```cpp
void SessionImpl::configureDeferred()
{
    if (!m_deferredConfigureScheduled) {
        QMetaObject::invokeMethod(this, [this]{ if (m_deferredConfigureScheduled) configure(); },
                                  Qt::QueuedConnection);
        m_deferredConfigureScheduled = true;
    }
}
```

`configure()`（`sessionimpl.cpp:1407`）会调用 `loadLTSettings()` + `configureComponents()` + 必要时 `reannounceToAllTrackers()`。

### 3.4 监听接口配置

`applyNetworkInterfacesSettings()`（`sessionimpl.cpp:2264`）负责把用户选择的网卡 IP / 端口组装成 libtorrent 的 `listen_interfaces` 字符串。libtorrent 期望格式为逗号分隔的 `IP:port` 列表（SSL 端口加 `s` 后缀）：

```
0.0.0.0:6881,[::]:6881,192.168.1.10:6882s
```

qBittorrent 在 Windows 上还要把网卡 friendly name 转成 GUID（`convertIfaceNameToGuid`），因为 libtorrent 在 Windows 上期望 GUID（`sessionimpl.cpp:2297-2316`）。`outgoing_interfaces` 也同步设置，影响所有出站 socket 的 bind。

---

## 4. SessionImpl 深度剖析

`SessionImpl` 是 qBittorrent 的 BT 核心，单一实例（`Session::initInstance` 友元），约 6981 行实现 + 917 行头文件，对外暴露 200+ 个虚函数。

### 4.1 关键成员

按职责分组（源 `sessionimpl.h:671-916`）：

**libtorrent 包装**：

- `lt::session *m_nativeSession`：裸指针，在析构时 `delete`（先调 `abort()` 拿到 `session_proxy`，移到独立线程析构）
- `NativeSessionExtension *m_nativeSessionExtension`：扩展插件，处理 fastresume_rejected、监听状态查询

**配置缓存（约 150 个 `CachedSettingValue`）**：覆盖 DHT/LSD/PeX、IP 过滤、磁盘 IO、加密、队列、限速、tracker、I2P、proxy 等所有 libtorrent 可调项。

**Torrent 索引**：

- `QHash<TorrentID, TorrentImpl *> m_torrents`：主索引
- `QHash<TorrentID, TorrentImpl *> m_hybridTorrentsByAltID`：v1/v2 混合 torrent 的备用 ID 索引（在 lt2 + hybrid 时同 torrent 可被 v1 SHA1 或 v2 SHA256 引用）
- `QHash<TorrentID, RemovingTorrentData> m_removingTorrents`：正在被删除的 torrent（异步等 libtorrent 删完文件）
- `QHash<TorrentID, TorrentID> m_changedTorrentIDs`：迁移期 ID 变更映射

**Alert 处理**：

- `std::vector<lt::alert *> m_alerts`：每次 `pop_alerts` 后的批量缓冲，`reserve(1024)` 预分配
- `QList<AddTorrentAlertHandler> m_addTorrentAlertHandlers`：FIFO 队列，每个 `async_add_torrent` 调用前 push 一个 lambda，alert 回来时 `takeFirst()` 取出执行。**这是把 libtorrent 异步 alert 串行化回 qBittorrent 同步期望的核心数据结构。**

**定时器**：

- `QTimer *m_seedingLimitTimer` (10s)：检查做种分享比/时间限制
- `QTimer *m_resumeDataTimer` (默认 60 分钟)：周期保存 resume data
- `QTimer *m_recentErroredTorrentsTimer` (1s)：清空 IO 错误 torrent 集合
- `QTimer *m_freeDiskSpaceCheckingTimer` (单次)：触发磁盘空间检查
- `QTimer *m_updateTrackersFromURLTimer` (24h)：定期从 URL 拉取额外 tracker

**工作线程**：

- `Utils::Thread::UniquePtr m_ioThread`：QThread，承载 FreeDiskSpaceChecker / FileSearcher / TorrentContentRemover
- `QThreadPool *m_asyncWorker`：单线程池，串行所有 libtorrent 异步调用
- `QPointer<FilterParserThread> m_filterParser`：IP 过滤解析
- `QPointer<BandwidthScheduler> m_bwScheduler`：带宽调度
- `QPointer<Tracker> m_tracker`：内嵌 tracker

### 4.2 Alert 派发机制

libtorrent 的 alert 是一种带类型的多态对象，所有事件（piece 完成、peer 连接、tracker 回复、stats 更新…）都通过 alert 队列传递。qBittorrent 的派发在 `readAlerts()`（`sessionimpl.cpp:6006`）：

```cpp
void SessionImpl::readAlerts()
{
    fetchPendingAlerts();                       // pop_alerts(&m_alerts)

    int previousAlertType = -1;
    qsizetype alertSequenceSize = 0;
    for (lt::alert *a : m_alerts) {
        const int alertType = a->type();
        // 同类型 alert 批量分摊：仅在类型变化时触发 endAlertSequence
        if (alertType != previousAlertType && previousAlertType != -1) {
            endAlertSequence(previousAlertType, alertSequenceSize);
            alertSequenceSize = 0;
        }
        handleAlert(a);                        // 大 switch
        ++alertSequenceSize;
        previousAlertType = alertType;
    }
    endAlertSequence(previousAlertType, alertSequenceSize);
    processPendingFinishedTorrents();           // 处理"finished"延迟触发
}
```

`handleAlert()`（`sessionimpl.cpp:6046-6175`）是一个 130 行的 `switch (alert->type())`，分派到 40+ 个 `handle*Alert()` 函数。Alert 类型分派规则：

- **torrent 生命周期**：`add_torrent_alert` → `handleAddTorrentAlert`（取 handler 执行）；`torrent_removed_alert`、`torrent_deleted_alert`、`torrent_delete_failed_alert`、`torrent_need_cert_alert`、`torrent_checked_alert`、`torrent_finished_alert`
- **状态更新**：`state_update_alert` → `handleStateUpdateAlert`（每 ~1.5s 一次，`post_torrent_updates` 触发）；`session_stats_alert` → `handleSessionStatsAlert`（计算速率、更新 `m_status`）
- **存储**：`storage_moved_alert`/`storage_moved_failed_alert`（移动存储任务完成）、`file_renamed_alert`/`file_rename_failed_alert`/`file_completed_alert`/`file_error_alert`/`fastresume_rejected_alert`
- **Resume data**：`save_resume_data_alert` → `handleSaveResumeDataAlert`（写盘）、`save_resume_data_failed_alert`
- **元数据**：`metadata_received_alert`
- **网络事件**：`listen_succeeded_alert`、`listen_failed_alert`、`external_ip_alert`（外部 IP 变化触发 reannounce）、`portmap_alert`/`portmap_error_alert`（UPnP/NAT-PMP）
- **Peer 事件**：`peer_blocked_alert`（5 种原因：ip_filter、port_filter、i2p_mixed、privileged_ports、utp_disabled、tcp_disabled）、`peer_ban_alert`、`url_seed_alert`、`ip_ban_alert`
- **Tracker**：`tracker_announce_alert`/`tracker_error_alert`/`tracker_reply_alert`/`tracker_warning_alert` → 统一 `handleTrackerAlert`
- **代理**：`socks5_alert`、`i2p_alert`
- **冲突**：`torrent_conflict_alert`（libtorrent 2.x 特有：v1/v2 hybrid torrent 同时存在）

`endAlertSequence` 的设计很巧妙：把"同类型 alert 批量触发一次副作用"做了 amortize。例如一批 `add_torrent_alert` 处理完后，统一发一次 `torrentsLoaded` 信号（避免每个 torrent 都触发 UI 更新）。

### 4.3 状态更新与统计

`session_stats_alert` 是 libtorrent 周期性（约每秒）推送的全局统计快照。qBittorrent 在 `handleSessionStatsAlert()`（`sessionimpl.cpp:6471`）里：

1. 通过 `m_metricIndices`（在 `initMetrics()` 里 `lt::find_metric_idx(name)` 一次性把字符串名转成 int 索引）
2. 取出 50+ 个 counter 值，计算速率：`calcRate = (current - previous) * 1e6 / interval_microseconds`
3. 更新 `SessionStatus m_status`（downloadRate/uploadRate/totalDownload/totalUpload/dhtNodes/peersCount/...）和 `CacheStatus m_cacheStatus`
4. 若 `totalDownload > m_status.totalDownload`：标记 `m_isStatisticsDirty = true`，按 `saveStatisticsInterval` 周期落盘

`state_update_alert` 由 `m_nativeSession->post_torrent_updates()` 触发，携带所有有变化的 torrent 的 `torrent_status`。`handleStateUpdateAlert()` 把每个 status 派给对应的 `TorrentImpl::handleStateUpdate()`，最后发 `torrentsUpdated` 信号给 GUI / WebUI。

### 4.4 关键方法：addTorrent_impl

`addTorrent_impl()`（`sessionimpl.cpp:2851-3156`，约 300 行）是新增 torrent 的主入口，处理：

1. **重复检测**：若 `findTorrent(infoHash)` 命中，根据 `isMergeTrackersEnabled()` 决定是否合并 tracker / url seeds（私有 torrent 永不合并）
2. **取消 metadata 下载**：若之前用 magnet 在后台下载 metadata，先 `cancelDownloadMetadata`
3. **构造 LoadTorrentParams**：从 `AddTorrentParams`（用户输入）+ 默认值（stopCondition / contentLayout / category 等）
4. **文件路径处理**：根据 `TorrentContentLayout`（Original / Subfolder / NoSubfolder）调整根目录
5. **文件名过滤**：若启用 `ExcludedFileNamesEnabled`，按正则过滤
6. **找未完成文件**：若 `!hasFinishedStatus`，调用 `findIncompleteFiles` 在 savePath 与 downloadPath 里搜索已存在的部分文件（避免重新下载）
7. **存 ResumeDataStorage**：`m_resumeDataStorage->store(torrentID, loadTorrentParams)`
8. **注入 ExtensionData**：`p.userdata = LTClientData(new ExtensionData)`，让 `NativeTorrentExtension` 构造时能拿到 status / trackers / urlSeeds 快照
9. **提交**：`m_nativeSession->async_add_torrent(p)`
10. **注册 handler**：`m_addTorrentAlertHandlers.append([...](alert){ createTorrent(...); })`

### 4.5 Resume Data 保存

周期保存由 `m_resumeDataTimer`（`generateResumeData()` → `saveResumeData()`，`sessionimpl.cpp:3323/3333`）触发：

```cpp
void SessionImpl::saveResumeData()
{
    for (TorrentImpl *const torrent : asConst(m_torrents))
        torrent->requestResumeData(lt::torrent_handle::save_info_dict);
    m_numResumeData = m_torrents.size();  // 等待 save_resume_data_alert
}
```

每个 `save_resume_data_alert` 回来时（`handleSaveResumeDataAlert`）会调用 `torrent->handleSaveResumeData(params)` → `m_resumeDataStorage->store(...)`。

### 4.6 Port Mapping

`enablePortMapping()` / `disablePortMapping()` / `addMappedPorts()` / `removeMappedPorts()`（`sessionimpl.cpp:3157-3227`）通过 `invokeAsync` 提交到 `m_asyncWorker` 单线程池执行（因为 `m_nativeSession->apply_settings` 不是线程安全，串行化到单线程池避免与主线程的 `configure()` 冲突）。`m_mappedPorts` 维护端口 → `vector<port_mapping_t>` 的映射，用于精确删除。

---

## 5. TorrentImpl 状态机

`TorrentImpl` 是单个 torrent 的管理类，3080 行实现。它包装 `lt::torrent_handle` + `lt::torrent_status`，提供 200+ 个属性访问器，并把 libtorrent 的 7 个内部状态映射到 qBittorrent 的 18 个 `TorrentState`：

### 5.1 TorrentState 枚举

| qBittorrent `TorrentState` | libtorrent `torrent_status::state_t` | 触发条件 |
|---|---|---|
| `Error` | (任意) | `m_nativeStatus.errc` 或 `upload_mode` flag 被设置 |
| `MissingFiles` | (任意) | `m_hasMissingFiles` 标志（fastresume 引用文件不存在但未做 reload） |
| `CheckingResumeData` | `checking_resume_data` | 加载 fastresume 时校验 |
| `CheckingDownloading` | `checking_files` + `!m_hasFinishedStatus` | 非完整 torrent 做完整性校验 |
| `CheckingUploading` | `checking_files` + `m_hasFinishedStatus` | 完整 torrent 做校验 |
| `DownloadingMetadata` | `downloading_metadata` + 非强制 | magnet 链接正在拉取 metadata |
| `ForcedDownloadingMetadata` | 同上 + `isForced()` | 强制模式拉 metadata |
| `Downloading` | `downloading` + `download_payload_rate > 0` | 正在下载且有数据 |
| `StalledDownloading` | `downloading` + 速率为 0 | 下载但停滞 |
| `ForcedDownloading` | `downloading` + `isForced()` | 强制下载（绕过队列） |
| `QueuedDownloading` | paused + auto_managed + 非停止 | 在下载队列等待 |
| `Uploading` | seeding + `upload_payload_rate > 0` | 正在做种 |
| `StalledUploading` | seeding + 速率为 0 | 做种但停滞 |
| `ForcedUploading` | seeding + `isForced()` | 强制做种 |
| `QueuedUploading` | paused + auto_managed + 完整 | 在做种队列等待 |
| `StoppedDownloading` | 显式 pause + 非完整 | 用户停止 |
| `StoppedUploading` | 显式 pause + 完整 | 用户停止做种 |
| `Moving` | （无对应 lt 状态） | storage move job 正在执行 |

### 5.2 updateState 状态转换图

核心在 `updateState()`（`torrentimpl.cpp:1289-1347`），每次 `handleStateUpdate` 或 `handleMoveStorageJobFinished` 等触发：

```
                  ┌──────────────────────┐
                  │  CheckingResumeData   │ ← async_add_torrent 后立即
                  └──────────┬───────────┘
                             ↓
                  ┌──────────┴───────────┐
                  │  hasMissingFiles?    │
                  └────┬────────────┬────┘
                       │            │
                       ↓            ↓
              ┌─────────────┐  ┌─────────────┐
              │MissingFiles │  │  hasError?  │
              └─────────────┘  └──┬──────────┘
                                  │
                                  ↓
                          ┌────────────────┐  yes
                          │  hasMetadata?  │──────→ Error
                          └──────┬─────────┘
                                 │ no
                                 ↓
                       ┌────────────────────────┐
                       │  isStopped?             │
                       │  └─ QueuedDownloading?  │
                       │     └─ Forced?          │
                       └──────┬─────────────────┘
                              ↓
                  DownloadingMetadata / ForcedDownloadingMetadata
                              │
                              │ metadata received
                              ↓
                  ┌────────────────────────────┐
                  │  checking_files && !stop?   │
                  └──────┬─────────────────────┘
                         │
                         ↓
              CheckingDownloading / CheckingUploading
                         │
                         ↓
                  ┌────────────────┐
                  │  isFinished?   │
                  └──┬──────────┬───┘
                     │ yes      │ no
                     ↓          ↓
            [Uploading 类]   [Downloading 类]
            依据 stopped/    依据 stopped/
            queued/forced/   queued/forced/
            upload_rate>0    download_rate>0
            分类 4 状态      分类 4 状态
```

### 5.3 start / stop / reload

- `start(mode)`（`torrentimpl.cpp:2033`）：clear_error → 若 `hasMissingFiles` 则 reload → 若 `m_isStopped` 则发 `started` 信号 → `setAutoManaged(mode==AutoManaged)` / `resume()`（强制）
- `stop()`（`torrentimpl.cpp:2014`）：`m_isStopped = true` → `deferredRequestResumeData()` → `setAutoManaged(false); pause()` → `m_payloadRateMonitor.reset()`
- `reload()`（`torrentimpl.cpp:1961`）：`m_nativeSession->remove(handle)` + `async_add_torrent(params)`（带 `ti`），相当于把 torrent 从 libtorrent 删了重新加，绕过 resume data
- `forceRecheck()`（`torrentimpl.cpp:1711`）：`m_nativeHandle.force_recheck()`，让 libtorrent 重新校验所有 piece

### 5.4 Metadata 接收

`handleMetadataReceivedAlert` 触发时，qBittorrent 需要把 libtorrent 刚拿到的 `torrent_info` 同步到 `m_torrentInfo`、`m_filePaths`、`m_filePriorities`，并通过 `endReceivedMetadataHandling()`（`torrentimpl.cpp:1891`）执行：

1. 重建 `m_indexMap`（libtorrent file_index → qBittorrent 顺序 index）
2. 应用 first/last piece priority
3. 调整存储位置（若 savePath 在 metadata 之前已设过）
4. 重新 `updateProgress()` + `updateState()`

---

## 6. BT 协议栈分层（libtorrent 内核）

libtorrent 2.x 的代码结构（`src/`）按层次组织：

### 6.1 网络层

| 组件 | 文件 | 职责 |
|---|---|---|
| TCP socket | `socket_type.hpp` + `instantiate_connection.cpp` | boost::asio tcp::socket，按需包装成 ssl_stream / socks5_stream / i2p_stream / utp_stream |
| uTP socket | `utp_stream.cpp` + `utp_socket_manager.cpp` + `packet_buffer.cpp` | BEP 29 uTP 协议，基于 LEDBAT 拥塞控制；`utp_socket_manager` 复用同一个 UDP socket 收发所有 uTP 连接，按 `connection_id` demux |
| SSL/TLS | `ssl.cpp` + `ssl_stream.hpp` | OpenSSL / mbedTLS 包装，支持 BEP 6 SSL torrent |
| I2P | `i2p_stream.cpp` + `i2p_pex.cpp` | 通过 SAM 桥接 I2P 网络 |
| WebSeed | `web_peer_connection.cpp` + `http_connection.cpp` | BEP 19 / BEP 17 HTTP/FTP 下载 |
| Tracker | `http_tracker_connection.cpp` / `udp_tracker_connection.cpp` / `websocket_tracker_connection.cpp` | 三种 tracker 协议 |

**uTP socket manager 关键流程**（`utp_socket_manager.cpp:114-200`）：

```cpp
incoming_packet(socket, ep, p):
  解析 utp_header（version、type、connection_id）
  按 (id, ep) 查找现有 utp_stream
  └─ 命中：转发到该 stream::incoming_packet
  └─ 未命中且 type==ST_SYN：
       检查 enable_incoming_utp
       检查 sockets 数量 < connections_limit*2（防 SYN flood）
       构造新 utp_stream（instantiate_connection）
       调用 m_cb(socket) 把新连接交给 session_impl::incoming_connection
```

### 6.2 协议层（Handshake & Message 流）

`bt_peer_connection`（3858 行）实现 BEP 3 BT 协议消息。核心入口：

**Handshake 发送**（`bt_peer_connection.cpp:715-798`）：1 字节长度 + "BitTorrent protocol" + 8 字节 reserved + 20 字节 info_hash + 20 字节 peer_id。Reserved 位含义：

- bit 7 (DHT)：`*(ptr+7) |= 0x01`
- bit 5 (Extension Protocol, BEP 10)：`*(ptr+5) |= 0x10`
- bit 7 (Fast Extension, BEP 6)：`*(ptr+7) |= 0x04`
- bit 7 (Hybrid v2, BEP 52)：`*(ptr+7) |= 0x10`（仅 v1 peer 在 hybrid torrent 上）

**消息分发**（`bt_peer_connection.cpp:2164`）：`switch(msg_id)` 分派到 `on_choke` / `on_unchoke` / `on_interested` / `on_not_interested` / `on_have` / `on_bitfield` / `on_request` / `on_piece` / `on_cancel` / `on_have_all` (BEP 6) / `on_have_none` (BEP 6) / `on_reject_request` (BEP 6) / `on_allowed_fast` (BEP 6) / `on_extended` (BEP 10) 等。

**peer_connection::start()**（`peer_connection.cpp:283`）负责初始化：
- 入站连接：socket 已经被 accept，`non_blocking(true)` 后调用 `init()`
- 出站连接：`m_socket.open(...) → set_traffic_class(DSCP) → bind_outgoing_socket → async_connect(on_connection_complete)`

**on_connection_complete**（`peer_connection.cpp:6321`）：标记 `m_connected = true`，更新 `m_local` endpoint，验证 outgoing interface 绑定正确，若为 uTP 则更新 peer_info 的 `confirmed_supports_utp`。

### 6.3 策略层（choking / piece picking / peer selection）

这是 libtorrent 的"智能"所在，分三个相互独立的算法：

#### 6.3.1 Choking 算法（`choker.cpp`，281 行）

入口 `unchoke_sort()`（`choker.cpp:186`），每秒由 `session_impl::recalculate_unchoke_slots` 调用一次：

```
1. 收集所有"可被 unchoke"的 peer：
   ─ 已连接 + 非 disconnecting + 非 connecting
   ─ 对端 interested（向我们请求过）
   ─ 非 web_seed + 非 ignore_unchoke_slots

2. 调用 unchoke_sort()：
   ─ 若 choking_algorithm == rate_based_choker：
       把 peers 按 upload_rate_compare 降序排序
       遍历，rate_threshold 从初始值开始递增 2KB/s
       遇到 rate < threshold 的就停止
       upload_slots = 已通过的 peer 数 + 1
   ─ 若 fixed_slots_choker：
       upload_slots = unchoke_slots_limit（用户设置）
   ─ 然后按 seed_choking_algorithm 选排序键：
       fastest_upload → uploaded_in_last_round 降序
       anti_leech     → anti_leech_score 升序（U 形曲线）
       round_robin    → 上传配额完成后降级，否则按上传量

3. 前 unchoke_set_size 个 unchoke，其余 choke
```

`compare_peers`（`choker.cpp:28`）是统一的预处理：

```cpp
int compare_peers(lhs, rhs):
    prio1 = lhs->get_priority(upload_channel)  // peer class priority
    prio2 = rhs->get_priority(upload_channel)
    if prio1 != prio2: return prio1 > prio2 ? 1 : -1
    
    c1 = lhs->downloaded_in_last_round()       // 我们从对方下载的字节
    c2 = rhs->downloaded_in_last_round()
    if c1 != c2: return c1 > c2 ? 1 : -1
    return 0  // 平手，交给具体算法（fastest_upload 用上传量、anti_leech 用分数）
```

`anti_leech_score`（`choker.cpp:120-153`）实现 "Improving BitTorrent: A Simple Approach"（Chow 等）的算法——给"刚开始下载"和"接近完成"的 peer 更高分（U 形曲线）：

```cpp
given_size = min(peer.total_payload_upload, total_size / 2)
have_size  = max(given_size, piece_length * peer.num_have_pieces)
score = abs((have_size - total_size/2) * 2000 / total_size)
// score 越高越好（接近 0% 或 100% 的 peer）
```

#### 6.3.2 乐观 Unchoke（`session_impl.cpp:4342-4475`）

每 `optimistic_unchoke_interval`（默认 ~30s）调用一次 `recalculate_optimistic_unchoke_slots()`：

1. 收集所有 choked + 对方 interested + 非连接中 + 非 disconnecting + torrent 未暂停 + 有 metadata 的 peer
2. 按 `last_optimistically_unchoked` 时间排序（最久没被优化的优先）
3. `num_optimistic_unchoke_slots = max(1, allowed_unchoke_slots / 5)`（约 1/5 的 slot 用于乐观）
4. `std::partial_sort` 取前 N 个，标记 `optimistically_unchoked = true` + `unchoke_peer()`
5. 之前被优化但现在没中的 peer：`choke_peer()`

#### 6.3.3 Peer 选择策略（`peer_list.cpp`，1476 行）

每个 torrent 有一个 `peer_list`，存储所有已知 peer（来自 tracker / DHT / PEX / LSD / resume data / incoming）。核心方法：

**`find_connect_candidates()`**（`peer_list.cpp:494`）：每秒被调用，找出最多 10 个值得尝试连接的 peer。算法：

```
round_robin = random 起点
for iterations in [0, min(peers.size, 300)]:
    pe = m_peers[round_robin++]
    if !is_connect_candidate(pe): continue
    
    # 退避：刚连过的 peer 不立即重试
    if pe.last_connected && session_time - pe.last_connected
       < (failcount + 1) * min_reconnect_time:
        continue
    
    # 插入候选列表（按 compare_peer 排序）
    insert_sorted(peers, pe, compare_peer)
    if peers.size() > 10:
        peers.resize(10)
```

**`is_connect_candidate()`**（`peer_list.cpp:478`）：

```cpp
if p.connection || p.banned || p.web_seed || !p.connectable
   || ((p.seed || p.upload_only) && m_finished)  // 完成后不连 seed
   || p.failcount >= m_max_failcount:
    return false
return true
```

**`compare_peer()`**（`peer_list.cpp:88`）——决定哪个候选更值得连：

```cpp
1. failcount 升序         // 失败次数少的优先
2. is_local() 降序        // 局域网 peer 优先
3. last_connected 升序    // 最久没连的优先
4. （seeding 时）maybe_upload_only 降序
5. source_rank 降序       // tracker > lsd > dht > pex
6. rank(external_ip) 降序  // 综合 IP 距离 + 端口
```

`source_rank`（`request_blocks.cpp:27-35`）的定义：

```cpp
ret |= (source & tracker)     ? 1 << 5 : 0  // 32
ret |= (source & lsd)         ? 1 << 4 : 0  // 16
ret |= (source & dht)        ? 1 << 3 : 0  // 8
ret |= (source & pex)        ? 1 << 2 : 0  // 4
// resume_data 和 incoming 不计入 rank
```

注意：**tracker 来的 peer 优先级最高（32）**，LSD 次之（16），DHT 再次（8），PEX 最低（4）。这是因为 PEX 是被动得到的（连接建立后才有），而 tracker 是显式 announce 拿到的，可信度更高。

**Ban / Failcount**：

```cpp
peer_list::inc_failcount(p): if (p->failcount < 31) ++p->failcount
peer_list::ban_peer(p):      p->banned = true  // 永久封禁，直到重启
```

`failcount` 在 `peer_connection::connect_failed()` 时递增（`peer_list.cpp:452`），5 位字段最大 31；在收到该 peer 的 tracker/DHT/PEX 通告时递减（被再次确认存在）。

### 6.4 存储层（disk_io_thread + cache）

libtorrent 2.x 的磁盘 IO 由 `disk_io_thread_pool`（`disk_io_thread_pool.cpp`）驱动，是一个**多线程任务池**，而非 1.x 的单线程 + aio 模型：

```cpp
disk_io_thread_pool::set_max_threads(N):
    若 N > current：直接 stop_threads 差额（线程自杀）
    若 N < current：先 eager 启 1 个线程保证 interrupt 有目标，
                    其余在 job_queued 时按需启动
```

线程在 `try_thread_exit()` 里检查 `m_threads_to_exit`，若 > 0 则 detach 自杀；空闲线程定时被 `reap_idle_threads` 清理。

`aio_threads` 默认 10，对应 qBittorrent 的 `m_asyncIOThreads` 设置。`hashing_threads`（默认 1）控制校验时并行度。

**Disk IO 任务类型**（在 `disk_job.hpp`）：

- `read` / `write`：常规读写
- `hash`：piece 哈希校验
- `move_storage`：跨目录移动
- `delete_files`：删除文件
- `rename_file`：重命名
- `flush_hashed`：把 hash 完成的 piece 从 cache flush 到磁盘
- `truncate`：截断文件到 piece 边界

任务之间通过 `disk_job_fence` 解决对同一 piece 的依赖（例如必须先 write 再 hash）。

`disk_cache`（`disk_cache.cpp`）维护 LRU 缓存，块大小 16 KiB（一个 block）。`mmap_disk_io` 是默认实现（libtorrent 2.x），把文件 mmap 到内存，配合 `posix_disk_io` / `pread_disk_io` 作为后备。

---

## 7. Peer 质量评分算法（伪代码 + 字段表 + 代码引用）

qBittorrent 本身**不实现 peer 评分**——它直接消费 libtorrent 的 `lt::peer_info` 结构。`PeerInfo` 类（`peerinfo.cpp`）只是把 `lt::peer_info` 字段映射为 Qt 风格的访问器，并计算一个"relevance"指标。

### 7.1 lt::peer_info 关键字段表

| 字段 | 类型 | 含义 | 用于 |
|---|---|---|---|
| `flags` | `peer_flags_t` (21 bit) | 见下表 | 状态显示 |
| `source` | `peer_source_flags_t` (6 bit) | 来源：tracker/dht/pex/lsd/resume_data/incoming | 排序权重 |
| `payload_up_speed` / `payload_down_speed` | int (B/s) | 实际传输速率（不含协议开销） | UI 显示、ETA |
| `up_speed` / `down_speed` | int | 包含协议开销的速率 | |
| `total_upload` / `total_download` | int64 | 累计传输字节 | 统计 |
| `progress` | float [0,1] | peer 拥有的 piece 比例 | seed 判定 |
| `pid` | peer_id (20B) | peer ID | 客户端识别 |
| `client` | string | 识别后的客户端名（如 "qBittorrent 4.5.0"） | UI |
| `connection_type` | connection_type_t | standard_bittorrent / web_seed / http_seed | UI 分类 |
| `rtt` | int (ms) | TCP connect 时测得的 RTT | 估算 |
| `num_pieces` | int | peer 拥有的 piece 数 | |
| `pieces` | bitfield | peer 持有的 piece 位图 | relevance 计算 |
| `download_queue_length` / `upload_queue_length` | int | 请求队列长度 | 流控 |
| `failcount` | int | 失败计数（5 位，最大 31） | 重连决策 |
| `downloading_piece_index` / `downloading_block_index` | int | 正在下载的 piece/block | UI 显示 |
| `optimistically_unchoked` | bool (flags bit 11) | 是否被乐观 unchoke | UI |
| `snubbed` | bool (flags bit 12) | 请求超时，被"snubbed" | 触发单 block 模式 |
| `on_parole` | bool (flags bit 9) | 曾发送坏 piece | 只请求整 piece |
| `seed` | bool (flags bit 10) | 是 seed | |
| `upload_only` | bool (flags bit 13) | BEP 27 上传只 | 决定是否连 |
| `endgame_mode` | bool (flags bit 14) | 在 endgame 模式 | |
| `holepunched` | bool (flags bit 15) | NAT 打洞成功 | |
| `utp_socket` / `ssl_socket` / `rc4_encrypted` / `plaintext_encrypted` | bool | 传输层信息 | UI flags |

### 7.2 peer_flags_t 位定义（`peer_info.hpp:92-194`）

| bit | 名称 | 含义 |
|---|---|---|
| 0 | `interesting` | 我们对其 piece 感兴趣 |
| 1 | `choked` | 我们已 choke 它 |
| 2 | `remote_interested` | 它对我们感兴趣 |
| 3 | `remote_choked` | 它 choke 了我们 |
| 4 | `supports_extensions` | 支持 BEP 10 |
| 5 | `outgoing_connection` | 我们发起的连接（vs 入站） |
| 6 | `handshake` | 正在握手 |
| 7 | `connecting` | 半开（正在 TCP connect） |
| 9 | `on_parole` | 在假释模式 |
| 10 | `seed` | 是 seed |
| 11 | `optimistic_unchoke` | 被乐观 unchoke |
| 12 | `snubbed` | 请求超时 |
| 13 | `upload_only` | 只上传（BEP 27） |
| 14 | `endgame_mode` | endgame 模式 |
| 15 | `holepunched` | NAT 打洞成功 |
| 16 | `i2p_socket` | I2P socket |
| 17 | `utp_socket` | uTP socket |
| 18 | `ssl_socket` | SSL socket |
| 19 | `rc4_encrypted` | RC4 加密 |
| 20 | `plaintext_encrypted` | DH 握手（未 RC4） |

### 7.3 PeerInfo::determineFlags（`peerinfo.cpp:301-387`）

把 `flags` 翻译成单字符显示串（类似 `D u U K ? O S I H X L E e P h`），用户在 GUI peers 列表看到的就是这串。映射规则：

```
interesting + remote_choked   → 'd'  (想下，但被对端 choke)
interesting + !remote_choked   → 'D'  (正在下载)
remote_interested + choked     → 'u'  (对方想下，被我们 choke)
remote_interested + !choked    → 'U'  (正在上传)
!remote_choked + !interesting → 'K'  (对方给我们 slot，但不需要)
!choked + !remote_interested   → '?'  (我们给对方 slot，但对方不需要)
optimistic_unchoke             → 'O'
snubbed                        → 'S'
!local_connection              → 'I'  (入站连接)
fromDHT                        → 'H'
fromPeX                        → 'X'
fromLSD                        → 'L'
rc4_encrypted                  → 'E'
plaintext_encrypted            → 'e'
utp_socket                     → 'P'
holepunched                    → 'h'
```

### 7.4 PeerInfo::calcRelevance（`peerinfo.cpp:285-294`）

qBittorrent 唯一自己计算的 peer 评分：

```cpp
qreal calcRelevance(const QBitArray &allPieces) const
{
    localMissing = allPieces.count(false);              // 我们没有的 piece 数
    if localMissing <= 0: return 0;
    peerPieces = pieces();                              // peer 持有的 piece 位图
    remoteHaves = (peerPieces & ~allPieces).count(true); // peer 有而我们没有的
    return qreal(remoteHaves) / localMissing;
}
```

含义：**peer 对我们的"有用程度" = (peer 有而我们没有的 piece 数) / (我们缺的 piece 总数)**。这是一个 [0,1] 区间的值，1.0 表示 peer 拥有我们所有缺失的 piece。这个值只用于 UI 显示，不参与决策——libtorrent 内部的 `piece_picker` 已经做了更精细的 piece-level 选择。

### 7.5 伪代码：Peer 综合评分（基于 libtorrent 内部逻辑）

把上述字段综合成一个"peer 健康度"评分，可指导 Rust 实现的 peer 优先级：

```
score(peer):
    # 1. 连接性（hard filter）
    if peer.banned or peer.failcount >= MAX_FAILCOUNT: return -∞
    if peer.connecting or peer.handshake: return -∞
    if (peer.seed or peer.upload_only) and we_are_seed: return -∞

    # 2. 速率（soft score，权重最高）
    rate_score = peer.payload_down_speed * 8  # B/s → bit/s
                 + peer.payload_up_speed * 2  # 上传贡献也计入

    # 3. 失败惩罚
    fail_penalty = peer.failcount * 1000

    # 4. 距离奖励
    if is_local(peer.address): +5000
    if is_same_subnet(peer.address, our_ip): +1000

    # 5. 来源可信度
    source_bonus = source_rank(peer.source) * 100
                   # tracker=32, lsd=16, dht=8, pex=4

    # 6. RTT 奖励（更低 RTT 更好）
    rtt_bonus = (peer.rtt > 0) ? max(0, 500 - peer.rtt) : 0

    # 7. 优化 unchoke 奖励（已经验证过通信）
    opt_bonus = peer.optimistic_unchoke ? 200 : 0

    return rate_score + source_bonus + rtt_bonus + opt_bonus
           - fail_penalty
```

### 7.6 Seed / Leech / Optimistic 识别

| 角色 | 判定条件 |
|---|---|
| **Seed** | `flags & seed` 或 `progress == 1.0` 或 `flags & upload_only`（BEP 27 显式声明） |
| **Leech** | `progress < 1.0` 且 `!(flags & upload_only)` |
| **Optimistic unchoke** | `flags & optimistic_unchoke` |
| **Snubbed**（超时但未断开） | `flags & snubbed`——libtorrent 内部会在请求超时后设置，进入"一次只请求一个 block"模式 |
| **On parole**（曾发送坏 piece） | `flags & on_parole`——只请求整 piece，不再切分 |
| **Endgame** | `flags & endgame_mode`——piece_picker 进入最后阶段，所有缺失 piece 同时向所有 peer 请求 |

---

## 8. 带宽分配模型

### 8.1 三层带宽限制结构

libtorrent 的带宽管理是**三层 quota 系统**：每个 socket 同时属于多个 `bandwidth_channel`，必须**所有 channel 都有 quota**才能发送。

```
                       ┌──────────────────────────┐
                       │  Global bandwidth_channel │  (session 级)
                       │   limit = download_speed_ │
                       │   limit / upload_speed_limit
                       └─────────────┬────────────┘
                                     │
              ┌──────────────────────┼──────────────────────┐
              │                      │                      │
       ┌──────▼──────┐        ┌──────▼──────┐        ┌─────▼─────┐
       │ TCP peer    │        │ uTP peer    │        │  Local    │
       │ class chan   │        │ class chan  │        │  class    │
       │ (throttle 0 │        │ (throttle 0 │        │ (限速豁免) │
       │  if prefer_  │        │  in prefer_ │        └───────────┘
       │  tcp mode)   │        │  tcp mode)  │
       └──────┬───────┘        └──────┬──────┘
              │                      │
              └──────────┬───────────┘
                         │
                  ┌──────▼──────┐
                  │ Per-torrent  │  (在 torrent 构造时 set_upload_limit)
                  │ bandwidth_   │
                  │   channel   │
                  └──────┬──────┘
                         │
                  ┌──────▼──────┐
                  │ Per-peer     │  (peer_connection 内置 channel)
                  │ bandwidth_   │
                  │   channel    │
                  └─────────────┘
```

### 8.2 bandwidth_channel / bandwidth_manager / bw_request

**`bandwidth_channel`**（`bandwidth_limit.cpp`，85 行）：

- `m_limit`：bytes/s 限额，0 表示无限
- `m_quota_left`：当前可用 quota（int64，避免溢出）
- `update_quota(dt_ms)`：每 tick 调用，`m_quota_left += m_limit * dt_ms / 1000`，最多累积到 `3 * m_limit`（约 3 秒）
- `use_quota(amount)`：消费 quota
- `return_quota(amount)`：连接断开时归还未用 quota
- `need_queueing(amount)`：若 `m_quota_left - amount < m_limit`（即"用掉这一笔会跌破单秒配额"）返回 true，需要进队列等待；否则直接扣减返回 false（fast path）

**`bandwidth_manager`**（`bandwidth_manager.cpp`，201 行）：

- 每个 channel（upload / download）一个 manager
- 维护 `std::vector<bw_request> m_queue`：等待 quota 分配的请求
- `request_bandwidth(peer, blk, priority, channels)`：
  - 若 channels 为空（peer 不属于任何限速 channel）→ 立即返回 `blk`（fast path）
  - 否则构造 `bw_request`，把需要排队的 channel 填进去，push 到队列尾，返回 0
- `update_quotas(dt)`：每 tick 调用：
  1. 移除已 disconnecting 的 peer 请求，归还 quota
  2. 对每个 channel 调用 `update_quota(dt_ms)` 补充 quota
  3. 遍历队列，每个 `bw_request::assign_bandwidth()` 尝试从所有关联 channel 各扣一点 quota（取最 limiting 的那个）
  4. 满足的 request 从队列移除，回调 `peer->assign_bandwidth(channel, assigned)`

**`bw_request`**（`bandwidth_queue_entry.hpp`）：

- `priority`：1 = 普通，>1 = 高优先级（web seed 用）
- `assigned`：已分配字节
- `request_size`：目标字节（满足后回调）
- `ttl`：最大轮数（在低限速下避免无限等待）
- `channel[max_bandwidth_channels=10]`：关联的 bandwidth_channel 数组

### 8.3 peer_class 系统

`peer_class`（`peer_class.cpp`）是限速的"分组抽象"：每个 peer_class 有独立的 upload/download throttle 和 priority。session 内置 5 个：

- `global_peer_class_id`：所有 peer 默认属于
- `tcp_peer_class_id`：所有 TCP peer
- `utp_peer_class_id`：所有 uTP peer
- `ssl_tcp_peer_class_id`、`ssl_utp_peer_class_id`：SSL 变体
- `local_peer_class_id`：局域网 peer（可选，由 `ignore_limits_on_lan` 决定）

qBittorrent 在 `configurePeerClasses()`（`sessionimpl.cpp:2333-2409`）里配置这些 class：

```cpp
lt::ip_filter f;
// 默认：所有 IPv4/IPv6 → global_peer_class
f.add_rule(any_v4, broadcast_v4, 1 << global_peer_class_id);
f.add_rule(any_v6, ffff::ffff, 1 << global_peer_class_id);

if (ignoreLimitsOnLAN):
    f.add_rule(10.0.0.0,    10.255.255.255, 1 << local_peer_class_id);
    f.add_rule(172.16.0.0,  172.31.255.255, 1 << local_peer_class_id);
    f.add_rule(192.168.0.0, 192.168.255.255, 1 << local_peer_class_id);
    f.add_rule(169.254.0.0, 169.254.255.255, 1 << local_peer_class_id);  // link-local
    f.add_rule(127.0.0.0,   127.255.255.255, 1 << local_peer_class_id);  // loopback
    # IPv6 类似：fe80::/10, fc00::/7, ::1

m_nativeSession->set_peer_class_filter(f);

# peer_class_type_filter：把 socket 类型映射到 class
lt::peer_class_type_filter typeFilter;
typeFilter.add(tcp_socket,        tcp_peer_class_id);
typeFilter.add(ssl_tcp_socket,    tcp_peer_class_id);
typeFilter.add(i2p_socket,        tcp_peer_class_id);
if (!isUTPRateLimited()):  # uTP 不受限
    typeFilter.disallow(utp_socket,    global_peer_class_id);
    typeFilter.disallow(ssl_utp_socket, global_peer_class_id);
m_nativeSession->set_peer_class_type_filter(typeFilter);
```

### 8.4 mixed_mode_algorithm（uTP/TCP 混合限速）

`session_impl::on_tick()`（`session_impl.cpp:3658-3707`）每秒根据 `mixed_mode_algorithm` 调整：

- **`prefer_tcp`**：uTP peer 不受全局限速影响，TCP peer 也无限速（两者都设 throttle=0）。简单但可能 uTP 占满带宽。
- **`peer_proportional`**：统计当前 TCP/uTP 的活跃 peer 数，按比例分配总限速：
  ```
  for channel in [upload, download]:
      total_peers = tcp_peers[channel] + utp_peers[channel]
      if utp_peers == 0 or total_peers < 5:
          tcp_limit = 0  # 不限速
      else:
          rate = current_stat_rate[channel]
          tcp_limit = max(rate * tcp_peers * 4 / total_peers, lower_limit)
          # lower_limit = upload 5KB/s, download 30KB/s
      set_rate_limit(tcp_peer_class, channel, tcp_limit)
  ```

### 8.5 qBittorrent 上下行控制

qBittorrent 的 `setGlobalDownloadSpeedLimit(limit)` / `setGlobalUploadSpeedLimit(limit)`（`sessionimpl.cpp:3595/3620`）：

1. 内部存的是 **KiB/s**（历史原因），对外暴露 **B/s**
2. `applyBandwidthLimits()`（`sessionimpl.cpp:1399`）组装一个临时 `settings_pack`，设置 `download_rate_limit` / `upload_rate_limit`，调 `apply_settings()`
3. Alt 速度（限速时段）通过切换 `m_isAltGlobalSpeedLimitEnabled` 来选择 normal / alt 限速值
4. BandwidthScheduler（30s 周期）根据时间表触发 `setAltGlobalSpeedLimitEnabled`

### 8.6 配置项映射表

| qBittorrent 设置 | libtorrent 路径 | 单位 | 默认 |
|---|---|---|---|
| `GlobalDLSpeedLimit` | `download_rate_limit` | B/s | 0 (无限) |
| `GlobalUPSpeedLimit` | `upload_rate_limit` | B/s | 0 |
| `MaxConnections` | `connections_limit` | 个 | 500 |
| `MaxUploads` | `unchoke_slots_limit` | 个 | 20 |
| `MaxConnectionsPerTorrent` | `add_torrent_params.max_connections` | 个 | 100 |
| `MaxUploadsPerTorrent` | `add_torrent_params.max_uploads` | 个 | 4 |
| `uTPRateLimited` | peer_class_type_filter.disallow(utp, global) | bool | true |
| `uTPMixedMode` | `mixed_mode_algorithm` | enum | prefer_tcp |
| `IncludeOverheadInLimits` | `rate_limit_ip_overhead` | bool | false |
| `IgnoreLimitsOnLAN` | peer_class_filter add local class | bool | false |
| `PeerToS` | `peer_dscp` | int | 0x01 |

---

## 9. 连接生命周期

### 9.1 状态机

```
                  ┌─────────────────┐
                  │   (peer 未知)    │
                  └────────┬────────┘
                           │
        ┌──────────────────┼──────────────────┐
        │ inbound           │ outbound
        ↓                  ↓
  ┌──────────┐       ┌──────────┐
  │ listening │       │ half_open│  (m_connecting=true)
  │ on socket │       │ async_   │
  │           │       │ connect  │
  └─────┬─────┘       └─────┬─────┘
        │ accept              │ TCP connect 完成
        ↓                    ↓
  ┌────────────────────────────────┐
  │  bt_peer_connection ctor       │
  │  ─ set_peer_classes            │
  │  ─ init() (if ready_for_conn)  │
  └────────────┬───────────────────┘
               │
               ↓
  ┌──────────────────────────────┐
  │  Handshake (exchange 68B)    │
  │  ─ write_handshake()         │
  │  ─ on_handshake() (receive)  │
  │  ─ verify info_hash match   │
  │  ─ identify peer_id → client│
  └────────────┬─────────────────┘
               │
               ↓
  ┌──────────────────────────────┐
  │  Extension handshake (BEP 10)│
  │  ─ on_extended_handshake     │
  │  ─ 各 plugin::on_extension_  │
  │    handshake (ut_pex,        │
  │    ut_metadata, lt_donthave)  │
  └────────────┬─────────────────┘
               │
               ↓
  ┌──────────────────────────────┐
  │  Bitfield exchange           │
  │  ─ 若支持 BEP 6:             │
  │      write_have_all() /      │
  │      write_have_none()       │
  │  ─ 否则: write_bitfield()    │
  └────────────┬─────────────────┘
               │
               ↓
  ┌──────────────────────────────┐
  │  Interested/Choke 协商        │
  │  ─ update_interest()         │
  │  │  遍历 m_have_piece        │
  │  │  若有任何我们缺的 piece:   │
  │  │     send_interested()     │
  │  │  否则: send_not_interested│
  │  ─ incoming_interested /     │
  │    incoming_choke /          │
  │    incoming_unchoke          │
  └────────────┬─────────────────┘
               │
               ↓
  ┌──────────────────────────────┐
  │  Data transfer (Request/Piece│
  │  循环)                       │
  │  ─ request_a_block() →      │
  │    piece_picker.pick_pieces  │
  │  ─ send_block_requests()     │
  │  ─ incoming_piece_fragment() │
  │  ─ block_received → hash →   │
  │    piece_passed/failed       │
  └────────────┬─────────────────┘
               │
               ↓  (任意时刻可触发)
  ┌──────────────────────────────┐
  │  Disconnect                  │
  │  ─ disconnect(ec, op, sev)   │
  │  ─ set_close_reason          │
  │  ─ remove from m_connections │
  │  ─ 若 outbound: inc_failcount│
  │  ─ 若 uTP 失败: try TCP      │
  │  ─ 若支持 holepunch: try      │
  └──────────────────────────────┘
```

### 9.2 入站连接：`session_impl::incoming_connection`

源码：`session_impl.cpp:3077-3321`。完整流程：

```cpp
incoming_connection(socket_type s):
    if m_abort or m_paused: return                 // 关闭中
    
    endp = s.remote_endpoint(ec)                   // 取对端地址
    if !enable_incoming_utp && is_utp(s):          # uTP 被禁
        post peer_blocked_alert(utp_disabled); return
    if !enable_incoming_tcp && is_tcp(s):          # TCP 被禁
        post peer_blocked_alert(tcp_disabled); return
    
    if !m_outgoing_interfaces.empty():
        local = s.local_endpoint()
        if !verify_incoming_interface(local.addr): reject
        if !verify_bound_address(local.addr):       reject
    
    if !is_local(endp.addr):
        m_stats_counters[has_incoming_connections] = 1
    
    # IP filter 检查（除非有 torrent 跳过过滤）
    if m_ip_filter && m_ip_filter->access(endp.addr) & blocked:
        post peer_blocked_alert(ip_filter); return
    
    # 连接数限制（按 peer class 的 connection_limit_factor 加权）
    peer_class_set pcs; set_peer_classes(&pcs, endp.addr, socket_type_idx(s))
    factor = max(pcs.connection_limit_factor) or 100
    limit = connections_limit * 100 / factor
    if num_connections() >= limit + connections_slack:
        post peer_disconnected_alert(too_many_connections); return
    
    # 若无活动 torrent 且无扩展要 on_unknown_torrent：拒绝
    if !incoming_starts_queued_torrents && !want_on_unknown_torrent:
        if !any_of(torrents, !is_paused): return
    
    # 构造 bt_peer_connection
    peer_connection_args pack{this, settings, stats_counters, disk_thread,
                              io_context, weak_torrent(), move(s), endp,
                              nullptr, generate_peer_id()};
    c = make_shared<bt_peer_connection>(pack)
    if !c.is_disconnecting:
        if num_connections() >= limit:
            c.peer_exceeds_limit()                # 标记需驱逐
        m_connections.insert(c)
        c.start()
```

### 9.3 出站连接：`torrent::connect_to_peer`

源码：`torrent.cpp:7962`。关键逻辑：

```cpp
connect_to_peer(torrent_peer* peerinfo, ignore_limit=false):
    peerinfo.last_connected = session_time()
    
    # 选择传输层
    if peerinfo.is_i2p_addr:
        socket = i2p_stream（走 SAM proxy）
    elif enable_outgoing_utp && (
            !enable_outgoing_tcp || peerinfo.supports_utp ||
            peerinfo.confirmed_supports_utp):
        sm = utp_socket_manager()    # uTP 优先
    elif !enable_outgoing_tcp:
        return false                  # 都禁了，无法连
    
    socket = instantiate_connection(io_context, proxy, ssl_ctx, sm)
    
    c = make_shared<bt_peer_connection>(args)
    peerinfo.connection = c
    m_connections.insert(c)
    c.start()
```

`peer_connection::start()`（`peer_connection.cpp:283`）里若 `m_connecting == true`：

```cpp
m_socket.open(m_remote.protocol())
set_traffic_class(m_socket, peer_dscp)
bind_outgoing_socket(m_socket, m_remote.address())
async_connect(m_remote, on_connection_complete)
```

### 9.4 超时与重试

`session_impl::on_tick`（`session_impl.cpp:3723-3771`）每秒扫描无关联 torrent 的连接（握手未完成）：

```cpp
for each connection in m_connections:
    if associated_torrent().expired(): continue  # 已关联 torrent 的由 torrent::second_tick 处理
    
    timeout = handshake_timeout
    # TCP 限制更严
    if is_tcp(socket): timeout = min(timeout, handshake_timeout / 2)
    
    if now - m_connect > seconds(timeout):
        connect_failed(timed_out)
```

`peer_connection::connect_failed`（`peer_connection.cpp:4194`）核心逻辑：

```cpp
# uTP 失败 → 立即用 TCP 重试
if is_utp(socket) && peer_info.supports_utp && !holepunch_mode:
    peer_info.supports_utp = false
    fast_reconnect(true)
    disconnect(e, connect, normal)
    post(ios, [t, self]{ t->connect_to_peer(pi, ignore_limit=true); })
    return

# 否则：标记 failure
disconnect(e, connect, failure)
# peer_list::inc_failcount 会被调用，达到阈值后不再尝试
```

### 9.5 封禁策略

**临时封禁**（`peer_list::inc_failcount`）：失败次数 +1，最大 31。`is_connect_candidate` 在 `failcount >= m_max_failcount` 时返回 false，不再连接。

**永久封禁**（`torrent::ban_peer`，`torrent.cpp:11884`）：

```cpp
bool torrent::ban_peer(torrent_peer* tp):
    if !m_peer_list->ban_peer(tp): return false
    if tp.connection: tp.connection->disconnect(errors::peer_banned, bittorrent, peer_error)
    return true

peer_list::ban_peer(p):
    if is_connect_candidate(*p): update_connect_candidates(-1)
    p.banned = true
    return true
```

封禁是 per-torrent 的，永久（直到 torrent 卸载）。常见触发场景：发送坏 piece（多次）、不响应请求。

### 9.6 NAT 穿透（Hole Punching）

libtorrent 实现 BEP 11（Holepunch extension）。流程（`peer_connection::connect_failed:4256-4271`）：

```cpp
# 若 TCP 连接失败 且 peer 支持 holepunch 且未在 holepunch 模式：
if (!is_utp || !enable_outgoing_tcp) && peer_info.supports_holepunch && !holepunch_mode:
    # 找一个已经连到该 peer 的 introducer
    introducer = t->find_introducer(remote)
    if introducer:
        # 让 introducer 转发 rendezvous 消息给目标 peer
        introducer->write_holepunch_msg(hp_rendezvous, remote)
        # 对端收到后会向我们的公网 IP:port 发起 uTP 连接
```

`peer_info::supports_holepunch` 由 BEP 10 extended handshake 中的 `holepunch` 字段设置。一旦打洞成功，`peer_info::holepunched` 标志置位（peer_info.hpp bit 15）。

### 9.7 uTP vs TCP 选择

libtorrent 的策略（`torrent::connect_to_peer:8031-8040`）：

```
1. 若 enable_outgoing_utp == true 且 (
       enable_outgoing_tcp == false   # 只能用 uTP
       OR peerinfo.supports_utp        # 已知支持 uTP（来自握手或 BEP 11 通告）
       OR peerinfo.confirmed_supports_utp  # 之前连过 uTP
   ):
    sm = utp_socket_manager()
2. 否则若 enable_outgoing_tcp:
    sm = nullptr (TCP)
3. 否则: return false  # 两个都禁
```

uTP 失败后会自动降级到 TCP（`connect_failed` 里 `peer_info.supports_utp = false`，下次 `connect_to_peer` 选 TCP）。

### 9.8 LSD / DHT / PEX 三大 peer 发现机制

| 机制 | 文件 | 协议 | 触发频率 |
|---|---|---|---|
| **LSD** | `lsd.cpp` | UDP 多播到 `239.192.152.143:6771`（IPv4）/ `[ff15::efc0:988f]:6771`（IPv6），消息体为 `BT-SEARCH * HTTP/1.1\r\nHost: ...\r\nPort: <port>\r\nInfohash: <hex>\r\ncookie: ...` | 每 5 分钟 + 重试 2 次（间隔 2s、4s） |
| **DHT** | `kademlia/*.cpp` | KRPC over UDP，BEP 5；`get_peers` 查询返回 peer 列表 | 自适应（torrent 数变化时调整间隔） |
| **PEX** | `ut_pex.cpp` | BEP 11 over BEP 10 extended message；每分钟发一次 `ut_pex` 消息，包含 added/dropped peer | 每 ~60s |

`ut_pex_plugin::tick()`（`ut_pex.cpp:87`）每分钟构造 PEX 消息：

```cpp
tick():
    if torrent.num_peers() == 0: return
    # 构造 added/dropped peers 列表（最近 60s 内连接的 / 断开的）
    # 限制每消息最多 50 个 added + 50 个 dropped（BEP 11 推荐）
    # 编码成 bencoded dict: {'added': <50×6 bytes>, 'added.f': <flags>, 'dropped': ..., ...}
    send_pex_message(payload)
```

`lsd::announce_impl`（`lsd.cpp:130`）多播 BT-SEARCH 并设置重试定时器：

```cpp
char msg[200];
render_lsd_packet(msg, sizeof(msg), listen_port, info_hash_hex, cookie, multicast_addr);
m_socket.send_to(buffer(msg), multicast_endpoint);
# 重试：retry_count < 3 时，每隔 2*retry_count 秒重发
```

---

## 10. 对 Rust 实现的启示

### 10.1 可直接借鉴的设计

| qBittorrent 设计 | Rust 实现建议 |
|---|---|
| **libtorrent alert callback → Qt slot 桥接** | 用 `tokio::sync::mpsc` 把 libtorrent-rs 的 alert 桥到 tokio task；或用 `crossbeam::channel` + `tokio::task::spawn_blocking` |
| **`m_asyncWorker` 单线程串行化 libtorrent 调用** | libtorrent-rs 的 `Session` 不是 `Sync`，必须用 `Mutex<Session>` 或 `tokio::sync::Mutex` + 单 worker task 串行所有 mutating 调用 |
| **`CachedSettingValue<T>` 模式** | Rust 用 `arc_swap::ArcSwap<T>` + `once_cell::sync::Lazy` 实现配置缓存；写入时 `arc_swap` 替换，读路径无锁 |
| **`SettingsStorage` 双文件原子写** | Rust 用 `tempfile::NamedTempFile` + `persist()`（rename），等价于 qBittorrent 的 `_new` 后缀方案 |
| **`NativeTorrentExtension` userdata 注入** | libtorrent-rs 的 `add_torrent_params::userdata` 是 `*mut c_void`，可用 `Box::into_raw` 注入；Rust 侧用 `Arc<ExtensionData>` + `Box::from_raw` 回收 |
| **`peer_class_filter` + `peer_class_type_filter`** | libtorrent-rs 暴露相同 API，直接调用 |
| **`BandwidthScheduler` 30s 定时检查** | `tokio::time::interval(Duration::from_secs(30))`，但要做时区处理（QTime / chrono） |
| **`SpeedMonitor` 30 样本 circular buffer** | `heapless::Deque<SpeedSample, 30>` 或 `std::collections::VecDeque` with capacity；维护运行和，O(1) 推入/弹出 |
| **`PortForwarderImpl` profile 抽象** | 用 `HashMap<String, HashSet<u16>>` 管理多 profile，便于 embedded tracker / BT session / WebUI 分别管理端口 |
| **`FilterParserThread` 后台解析 IP 过滤** | `tokio::task::spawn_blocking` 解析 DAT/P2P/P2B 文件，结果 `ip_filter` 通过 channel 回主任务 |

### 10.2 需改造的设计

| qBittorrent 设计 | 问题 | Rust 改造建议 |
|---|---|---|
| **`QHash<TorrentID, TorrentImpl*>` 裸指针** | C++ 用 Qt 父子对象树管理生命周期，但裸指针容易悬空（实际 qBittorrent 用 `QPointer` 缓解） | Rust 用 `Arc<Mutex<TorrentImpl>>` 或 `Arc<RwLock<>>`；若要避免锁，用 `tokio::sync::RwLock` + `Arc` |
| **`CachedSettingValue<T>` 隐式同步** | C++ 用 `QReadWriteLock`，Rust 不能直接套——配置变更需要通知订阅者 | 用 `arc_swap::ArcSwap<T>` + `tokio::sync::watch::channel<T>`（watch 可让多个订阅者收到变更通知） |
| **`m_nativeSession->apply_settings()` 同步阻塞** | libtorrent 内部会重建 socket，可能耗时数百毫秒 | 在专用 `tokio::task::spawn_blocking` 里执行，避免阻塞 runtime |
| **alert 处理在 Qt 主线程** | libtorrent 网络 IO 在自己线程，alert 派发到主线程，需要锁 | 若用 libtorrent-rs，alert 可直接在网络线程处理（无 Qt 桥接），但要注意不要在 alert handler 里调用 `Session` 的同步方法（会死锁） |
| **`NativeTorrentExtension::on_state` 里调用 `pause()`** | libtorrent 内部回调里调用 libtorrent API 是合法的，但 Rust FFI 边界要小心 | 用 `SessionHandle` 弱引用，必要时 `post` 到独立任务执行 |
| **GUI / WebUI 共享 `TorrentImpl`** | Qt 用信号槽广播变更，Rust 没有 | 用 `tokio::sync::broadcast` 或 `arc_swap::ArcSwap<Arc<HashMap<TorrentID, TorrentSnapshot>>>` 做 CQRS：写路径更新 map，读路径无锁克隆 |

### 10.3 关键陷阱

1. **libtorrent 2.x 的 `userdata` 类型变了**：从 `void*` 变成 `lt::client_data_t`（强类型 wrapper）。qBittorrent 用 `#ifdef QBT_USES_LIBTORRENT2` 区分；Rust 应直接绑定 libtorrent 2.x，放弃 1.x 兼容。

2. **`async_add_torrent` 的 alert 顺序不保证**：多个 `async_add_torrent` 调用产生的 `add_torrent_alert` 可能乱序到达。qBittorrent 用 `m_addTorrentAlertHandlers` FIFO 队列匹配——这假设"alert 到达顺序 = 提交顺序"，但 libtorrent 文档未明确保证。**Rust 实现应改用 info_hash 匹配**，避免依赖顺序。

3. **`fastresume_rejected_alert` 的副作用**：`NativeSessionExtension::on_alert` 里在 fastresume 被拒时自动 `unset_flags(auto_managed); pause();`。这是 qBittorrent 的"安全降级"策略——避免 libtorrent 用错误的 fastresume 数据继续下载导致数据损坏。**Rust 必须保留这个降级逻辑**，否则会出现"fastresume 引用了已不存在的文件路径，但 libtorrent 仍尝试写盘"的情况。

4. **`peer_turnover` 系列参数**：libtorrent 默认每 300s 主动断开部分 peer 以引入新 peer（避免 peer 集合僵化）。qBittorrent 暴露了 `peerTurnover` (4)、`peerTurnoverCutoff` (90)、`peerTurnoverInterval` (300) 三个参数。**Rust 实现应保留默认值**，否则会陷入"只连老 peer，新 peer 进不来"的退化状态。

5. **`mixed_mode_algorithm` 的 `peer_proportional` 模式**：libtorrent 的实现假设 TCP 和 uTP 共享同一物理带宽，按活跃 peer 数比例分配。但若你的网络是 uTP 友好型（如家庭宽带），这个算法会过度限制 uTP。**Rust 实现应让用户可选**，并默认 `prefer_tcp`。

6. **`anonymous_mode`**：启用后会禁用 DHT、LSD、PEX，改 User-Agent，不向 tracker 发 `ip` 参数。**Rust 实现务必提供这个开关**，否则在某些司法管辖区（如德国）会有法律风险。

7. **`save_resume_data` 的"info_dict 是否保存"选项**：libtorrent 2.x 的 `save_resume_data_alert` 默认不返回 torrent 的 `info_dict`（仅返回 fastresume 数据）。qBittorrent 在 `requestResumeData` 时显式传 `lt::torrent_handle::save_info_dict` flag。**Rust 必须传这个 flag**，否则 resume data 不自包含，丢失 `.torrent` 文件后无法恢复。

8. **`BencodeResumeDataStorage` vs `DBResumeDataStorage` 的取舍**：Bencode 是每 torrent 一个 `.fastresume` 文件 + 一个 `.torrent` 文件，启动时需遍历目录、解析每个文件——上千 torrent 时启动耗时数秒。DBResumeDataStorage 用 SQLite 单库，启动只需 `SELECT * FROM torrents`，但写入是单点（SQLite 全局锁）。**Rust 推荐用 SQLite**（`rusqlite` crate），并考虑 WAL 模式提升并发。

9. **`BandwidthScheduler` 的时间判定**：qBittorrent 用 `QTime::currentTime()`，依赖系统时区。若系统时区在运行时被修改，调度会出错。**Rust 用 UTC + 用户配置的"本地时段"，避免依赖系统时区**。

10. **`m_ioThread` 的 QObject 父子树**：qBittorrent 把 `FreeDiskSpaceChecker`、`FileSearcher`、`TorrentContentRemover` 都 moveToThread 到 `m_ioThread`。**Rust 不需要这种 hack**——直接 `tokio::task::spawn_blocking` 即可，每个任务独立线程池管理。

### 10.4 Rust 原型架构建议

基于以上分析，推荐的 Rust 原型架构：

```
┌─────────────────────────────────────────────────────────┐
│                  Multi-Downloader (Rust)                │
│                                                         │
│  ┌──────────────────────────────────────────────────┐  │
│  │  Tokio runtime (multi-thread, 1 worker per core) │  │
│  │                                                  │  │
│  │  ┌────────────────────────────────────────────┐  │  │
│  │  │  BtSession task (single task, owns Session) │  │  │
│  │  │  ─ libtorrent-rs::Session                  │  │  │
│  │  │  ─ pop_alerts loop (in this task)          │  │  │
│  │  │  ─ 所有 mutating 调用都在这里串行执行       │  │  │
│  │  │  ─ alert 派发给订阅者 via broadcast        │  │  │
│  │  └────────────────────────────────────────────┘  │  │
│  │                                                  │  │
│  │  ┌──────────────────┐  ┌──────────────────────┐ │  │
│  │  │ ConfigStore      │  │ WebUITask (axum)     │ │  │
│  │  │ ArcSwap<HashMap> │  │ 读 ConfigStore +     │ │  │
│  │  │ + watch::channel │  │ 调 BtSession command │ │  │
│  │  └──────────────────┘  └──────────────────────┘ │  │
│  │                                                  │  │
│  │  ┌──────────────────────────────────────────┐    │  │
│  │  │ BtCommand mpsc channel → BtSession task  │    │  │
│  │  │ (AddTorrent / RemoveTorrent / SetSetting)│    │  │
│  │  └──────────────────────────────────────────┘    │  │
│  │                                                  │  │
│  │  ┌──────────────────┐  ┌──────────────────────┐ │  │
│  │  │ ResumeStorage    │  │ FilterParser task     │ │  │
│  │  │ (SQLite + WAL)   │  │ (spawn_blocking)      │ │  │
│  │  └──────────────────┘  └──────────────────────┘ │  │
│  └──────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────┘
```

关键点：

- **`BtSession` 单 task**：所有 libtorrent 调用都在同一个 tokio task 里，避免 `Send` 问题。其他 task 通过 `mpsc::Sender<BtCommand>` 提交命令。
- **alert 用 `broadcast`**：`BtSession` 把 alert clone 后发到 `broadcast::Sender<Arc<Alert>>`，WebUI / GUI / 日志各自订阅。
- **配置用 `arc_swap` + `watch`**：读路径无锁（`ArcSwap::load`），写路径替换原子指针；变更通过 `watch::channel` 通知需要响应变更的组件（如 BandwidthScheduler）。
- **Resume data 用 SQLite**：`rusqlite` + `r2d2` 连接池，WAL 模式。表结构参考 `dbresumedatastorage.cpp` 的 24 列 schema。
- **磁盘 IO 用 `spawn_blocking`**：libtorrent 自带 disk_io_thread_pool，但额外的文件操作（如移动 savePath、删除内容）用 `tokio::task::spawn_blocking`。

---

## 附录 A：关键类/函数速查表

### qBittorrent 侧

| 类/函数 | 文件:行 | 角色 |
|---|---|---|
| `BitTorrent::Session` | `session.h:138` | 抽象接口 |
| `BitTorrent::SessionImpl` | `sessionimpl.h:144` | 实现（6981 行 cpp） |
| `BitTorrent::TorrentImpl` | `torrentimpl.h` / `torrentimpl.cpp:307` | 单 torrent 管理 |
| `BitTorrent::PeerInfo` | `peerinfo.h` / `peerinfo.cpp` | lt::peer_info 包装 |
| `BitTorrent::BencodeResumeDataStorage` | `bencoderesumedatastorage.cpp:110` | 文件式 resume data |
| `BitTorrent::DBResumeDataStorage` | `dbresumedatastorage.cpp` | SQLite resume data |
| `BitTorrent::NativeSessionExtension` | `nativesessionextension.cpp:51` | session 级 lt 插件 |
| `BitTorrent::NativeTorrentExtension` | `nativetorrentextension.cpp:33` | torrent 级 lt 插件 |
| `BitTorrent::FilterParserThread` | `filterparserthread.h:39` | IP 过滤解析线程 |
| `BitTorrent::PortForwarderImpl` | `portforwarderimpl.cpp:35` | UPnP/NAT-PMP |
| `BandwidthScheduler` | `bandwidthscheduler.cpp:42` | 时段限速 |
| `SpeedMonitor` | `speedmonitor.h:70` | 30 样本 circular buffer |
| `Application` | `application.cpp:293` | 主程序单例 |
| `SettingsStorage` | `settingsstorage.cpp:50` | key-value 配置存储 |
| `CachedSettingValue<T>` | `settingvalue.h:67` | 配置缓存模板 |

### libtorrent 侧

| 类/函数 | 文件:行 | 角色 |
|---|---|---|
| `lt::session` | `session.cpp` / `session_impl.cpp` | 主 session |
| `lt::torrent` | `torrent.cpp` | 单 torrent 内部表示 |
| `lt::peer_connection` | `peer_connection.cpp:101` | peer 连接基类 |
| `lt::bt_peer_connection` | `bt_peer_connection.cpp` | BT 协议 peer |
| `lt::peer_list` | `peer_list.cpp:125` | per-torrent peer 池 |
| `lt::piece_picker` | `piece_picker.cpp` / `aux_/piece_picker.hpp` | piece 选择 |
| `lt::aux::bandwidth_channel` | `bandwidth_limit.cpp:18` | 带宽 quota |
| `lt::aux::bandwidth_manager` | `bandwidth_manager.cpp:20` | quota 分配器 |
| `lt::aux::unchoke_sort` | `choker.cpp:186` | choking 决策 |
| `lt::peer_class` | `peer_class.cpp:16` | peer 分组 + 限速 |
| `lt::disk_io_thread_pool` | `disk_io_thread_pool.cpp:36` | 磁盘 IO 线程池 |
| `lt::utp_socket_manager` | `utp_socket_manager.cpp:48` | uTP 管理 |
| `lt::lsd` | `lsd.cpp` | Local Service Discovery |
| `lt::ut_pex_plugin` | `ut_pex.cpp:55` | Peer Exchange |
| `lt::plugin` / `torrent_plugin` / `peer_plugin` | `extensions.hpp:166/280/367` | 扩展接口 |

---

## 附录 B：libtorrent settings_pack 完整清单（qBittorrent 使用的子集）

按类别分组，括号内为 qBittorrent 配置键名：

### 网络与监听

| settings_pack 字段 | 默认 | 来源配置 |
|---|---|---|
| `listen_interfaces` | (动态) | `Interface` + `Port` |
| `outgoing_interfaces` | (动态) | 同上 |
| `listen_queue_size` | 30 | `SocketBacklogSize` |
| `send_socket_buffer_size` | 0 (auto) | `SocketSendBufferSize` |
| `recv_socket_buffer_size` | 0 (auto) | `SocketReceiveBufferSize` |
| `max_retry_port_bind` | 0 | (port > 0 时设) |
| `listen_system_port_fallback` | false | (硬编码) |

### Peer 协议

| 字段 | 默认 | 来源 |
|---|---|---|
| `enable_incoming_tcp` / `outgoing_tcp` | true | `BTProtocol` |
| `enable_incoming_utp` / `outgoing_utp` | true | `BTProtocol` |
| `mixed_mode_algorithm` | prefer_tcp | `uTPMixedMode` |
| `allow_multiple_connections_per_ip` | false | `MultiConnectionsPerIp` |
| `allow_multiple_connections_per_pid` | false | `MultiConnectionsPerPeerID` |
| `no_connect_privileged_ports` | false | `BlockPeersOnPrivilegedPorts` |
| `peer_dscp` | 0x01 | `PeerToS` |
| `peer_turnover` / `peer_turnover_cutoff` / `peer_turnover_interval` | 4 / 90 / 300 | 同名 |
| `connection_speed` | 30 | `ConnectionSpeed` |
| `seeding_outgoing_connections` | true | `SeedingOutgoingConnectionsEnabled` |

### 加密

| 字段 | 默认 | 来源 |
|---|---|---|
| `out_enc_policy` / `in_enc_policy` | enabled | `Encryption` (0=enabled, 1=forced, 2=disabled) |
| `allowed_enc_level` | pe_rc4 | (硬编码最严) |
| `prefer_rc4` | true | (硬编码) |

### 限速

| 字段 | 默认 | 来源 |
|---|---|---|
| `download_rate_limit` / `upload_rate_limit` | 0 | `GlobalDLSpeedLimit` / `GlobalUPSpeedLimit` |
| `connections_limit` | 500 | `MaxConnections` |
| `unchoke_slots_limit` | 20 | `MaxUploads` |
| `rate_limit_ip_overhead` | false | `IncludeOverheadInLimits` |

### 队列

| 字段 | 默认 | 来源 |
|---|---|---|
| `active_downloads` | -1 (off) | `MaxActiveDownloads` (3, 若 queueing enabled) |
| `active_seeds` | -1 | `MaxActiveUploads` (3) |
| `active_limit` | -1 | `MaxActiveTorrents` (5) |
| `dont_count_slow_torrents` | false | `IgnoreSlowTorrentsForQueueing` |
| `inactive_down_rate` | 2048 | `SlowTorrentsDownloadRate` (2 KiB/s → B) |
| `inactive_up_rate` | 2048 | `SlowTorrentsUploadRate` |
| `auto_manage_startup` | 60 | `SlowTorrentsInactivityTimer` |
| `active_checking` | 1 | `MaxActiveCheckingTorrents` |

### 磁盘 IO

| 字段 | 默认 | 来源 |
|---|---|---|
| `aio_threads` | 10 | `AsyncIOThreadsCount` |
| `hashing_threads` | 1 | `HashingThreadsCount` |
| `file_pool_size` | 100 | `FilePoolSize` |
| `checking_mem_usage` | 2048 (32×64KiB) | `CheckingMemUsageSize` |
| `cache_size` (1.x) / 无(2.x) | -1 / n/a | `DiskCacheSize` |
| `cache_expiry` (1.x) | 60 | `DiskCacheTTL` |
| `max_queued_disk_bytes` | 104857600 | `DiskQueueSize` |
| `disk_io_read_mode` / `disk_io_write_mode` | enable_os_cache | `DiskIOReadMode` / `DiskIOWriteMode` |
| `coalesce_reads` / `coalesce_writes` (1.x) | false (Linux) / true (Win) | `CoalesceReadWrite` |
| `piece_extent_affinity` | false | `PieceExtentAffinity` |
| `suggest_mode` | no_piece_suggestions | `SuggestMode` |
| `send_buffer_watermark` / `low_watermark` / `factor` | 512000 / 10240 / 50 | 同名 (×1024) |
| `mmap_file_size_cutoff` (2.x) | INT_MAX (强制 pread) | `DiskIOType=SimplePreadPwrite` 时 |
| `disk_write_mode` (2.x) | always_pwrite | 同上 |

### Tracker

| 字段 | 默认 | 来源 |
|---|---|---|
| `announce_to_all_trackers` | false | `AnnounceToAllTrackers` |
| `announce_to_all_tiers` | true | `AnnounceToAllTiers` |
| `announce_ip` | "" | `AnnounceIP` |
| `announce_port` (2.x) | 0 | `AnnouncePort` |
| `max_concurrent_http_announces` | 50 | `MaxConcurrentHTTPAnnounces` |
| `stop_tracker_timeout` | 2 | `StopTrackerTimeout` |
| `auto_scrape_interval` | 1200 | (硬编码) |
| `auto_scrape_min_interval` | 900 | (硬编码) |
| `validate_https_trackers` | true | `ValidateHTTPSTrackerCertificate` |
| `ssrf_mitigation` | true | `SSRFMitigation` |
| `apply_ip_filter_to_trackers` | false | `TrackerFilteringEnabled` |

### DHT / LSD

| 字段 | 默认 | 来源 |
|---|---|---|
| `enable_dht` | true | `DHTEnabled` |
| `enable_lsd` | true | `LSDEnabled` |
| `dht_bootstrap_nodes` | (官方) | `DHTBootstrapNodes` |
| `use_dht_as_fallback` | false | (硬编码) |

### 代理

| 字段 | 默认 | 来源 |
|---|---|---|
| `proxy_type` | none | `Net::ProxyConfiguration` (HTTP/SOCKS4/SOCKS5) |
| `proxy_hostname` / `proxy_port` | "" | 同上 |
| `proxy_username` / `proxy_password` | "" | (若 authEnabled) |
| `proxy_peer_connections` | false | `ProxyPeerConnections` |
| `proxy_hostnames` | false | `hostnameLookupEnabled` |

### I2P (2.x)

| 字段 | 默认 | 来源 |
|---|---|---|
| `i2p_hostname` / `i2p_port` | "" / 0 | `I2P/Address` / `I2P/Port` |
| `allow_i2p_mixed` | false | `I2P/MixedMode` |
| `i2p_inbound_quantity` / `outbound_quantity` | 3 / 3 | 同名 |
| `i2p_inbound_length` / `outbound_length` | 3 / 3 | 同名 |

### UPnP / NAT-PMP

| 字段 | 默认 | 来源 |
|---|---|---|
| `enable_upnp` / `enable_natpmp` | false → 动态 true | PortForwarderImpl 控制 |
| `upnp_lease_duration` / `natpmp_lease_duration` | 0 | `UPnPLeaseDuration` |

### 杂项

| 字段 | 默认 | 来源 |
|---|---|---|
| `anonymous_mode` | false | `AnonymousModeEnabled` |
| `user_agent` | "qBittorrent/x.y.z" | (硬编码) |
| `peer_fingerprint` | "-qBxyza-" | (硬编码, lt::generate_fingerprint) |
| `resolver_cache_timeout` | 1200 | `HostnameCacheTTL` |
| `allow_idna` | false | `IDNSupportEnabled` |
| `alert_mask` | (composite) | (含 error/file_progress/ip_block/peer/port_mapping/status/storage/tracker + 可选 performance) |
| `alert_queue_size` | INT_MAX/2 | (硬编码防丢) |
| `active_tracker_limit` / `active_dht_limit` / `active_lsd_limit` | -1 | (硬编码无限) |
| `enable_set_file_valid_data` (2.x) | true | (硬编码) |
| `metadata_token_limit` (2.x) | (Preferences) | `getBdecodeTokenLimit` |

---

*文档完。本文档基于 qBittorrent master 分支（约 v5.0 系列）与 libtorrent RC_2_0 分支静态分析生成。所有代码引用均带文件名与行号，便于交叉验证。*
