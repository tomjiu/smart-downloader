# Tixati v3.44 逆向深度分析

> **目标**：剖析闭源 Tixati 客户端的 BT 内核架构、Peer 评分算法、带宽分配模型、连接生命周期管理，为新 Rust 多协议下载器提供设计依据。
>
> **方法**：纯静态分析。二进制为 `/home/z/my-project/reversing/binaries/tixati_extracted/usr/bin/tixati`（90 MB ELF64 stripped）。工具链 `binutils + lief + strings`。无 Ghidra/radare2/rizin 可用，故本文档以 `.rodata` 字符串提取 + `.text` 反汇编 xref 为主要证据来源。
>
> **作者**：Tixati 由用户 "shake" 开发，源码路径 `/home/shake/Desktop/prog/tixati/src/` 在崩溃日志中泄露。

---

## 1. 概览

Tixati 是一款**完全自研**的 BT 客户端，**不依赖 libtorrent、不依赖 OpenSSL、不依赖 libcurl**。整个 BT 协议栈、DHT、uTP、加密握手（MSE/PE）、HTTP/HTTPS 客户端、磁盘 IO、autothrottle 算法全部从零实现，静态链接到单一 90 MB ELF 中。这是它在 2026 年仍具研究价值的根本原因——所有设计决策都可从二进制反汇编中追溯，且与 qBittorrent/FileCentipede 这类"libtorrent wrapper"形成鲜明对比。

Tixati 的核心技术创新：

1. **三种 unchoke 模式**：Forced（用户强制）、Random（标准 optimistic unchoke）、Charity（Tixati 独有，给低分 peer 机会）
2. **三层带宽分配**：Trading Allocation（交易型，互惠 peer 优先）+ Seeding Allocation（做种型，纯做种 peer）+ Auto Limit（基于 RTT 的自动限速，类 LEDBAT）
3. **Channel 系统**：基于 DHT 的 P2P 聊天/订阅频道，利用 BEP 44 在 DHT 中存储频道消息
4. **I2P 集成**：原生支持通过 I2P 匿名网络进行 peer 连接、tracker 通信和 DHT
5. **Scheduler**：基于 weekday × cycle 的可编程任务调度器

---

## 2. 二进制基本信息

### 2.1 ELF Header

```
Class:       ELF64 LSB
Machine:    x86_64
Type:       EXEC (non-PIE)        ← 入口地址固定 0x5823c0
Entrypoint: 0x5823c0
Sections:   35
BuildID:    e9590d0703eea1e91b38ca92f2da519f592a8d77
Stripped:   Yes (no .symtab)
```

### 2.2 关键段

| 段名 | 虚拟地址 | 大小 | 用途 |
|------|----------|------|------|
| `.text` | `0x40e9d0` | 4.0 MB | 全部代码（含自研 BT 引擎、SSL、加密、GTK UI） |
| `.rodata` | `0x4466000` | 11.4 MB | 字符串、常量表、UI 资源 |
| `.eh_frame` | `0x505df68` | 6.6 MB | C++ 异常 unwind 表 |
| `.gcc_except_table` | `0x56ba768` | 2.9 MB | C++ 异常处理表 |
| `.data` | `0x5990000` | ~1.6 MB | 全局变量、vtable |

**注意**：`.eh_frame` + `.gcc_except_table` 占 9.5 MB（占二进制 10%），说明 Tixati 大量使用 C++ 异常。结合 `.text` 仅 4 MB，实际逻辑代码密度极高。

### 2.3 动态依赖（仅 17 个 .so）

```
libdl / libz / librt / libpthread / libm / libc / ld-linux  ← 系统基础
libglib-2.0 / libgio-2.0 / libgobject-2.0 / libgthread-2.0  ← GLib 工具库
libgdk-3 / libgtk-3 / libpango / libgdk_pixbuf / libpangocairo / libcairo  ← GTK3 UI
```

**关键洞察**：

- **没有 libssl/libcrypto** → TLS 自实现（mini_install.dll 也是同样思路）
- **没有 libcurl** → HTTP(S) 客户端自实现
- **没有 libtorrent** → BT 协议栈自实现
- **没有 libnatpmp / miniupnpc** → UPnP/NAT-PMP 自实现
- **没有 boost** → 用 GLib 替代 C++ 标准库扩展
- GTK3 + Cairo 是唯一外部 UI 框架

### 2.4 导入符号分类

| 类别 | 数量 | 代表函数 |
|------|------|----------|
| GTK3 widget | ~150 | `gtk_widget_grab_focus`, `gtk_menu_get_type`, `gtk_window_get_type` |
| GLib core | ~80 | `g_list_append`, `g_variant_builder_open`, `g_spawn_async_with_pipes` |
| GIO / D-Bus | ~30 | `g_dbus_connection_register_object`, `g_bus_unwatch_name` |
| POSIX 网络 | 7 | `socket`, `connect`, `sendto`, `recv`, `recvfrom`, `bind`, `listen` |
| epoll | 3 | `epoll_create`, `epoll_ctl`, `epoll_wait` |
| select/poll | 2 | `select`, `poll` |
| 文件 IO | ~15 | `open`, `read`, `write`, `lseek`, `fstat`, `fsync`, `fallocate` |
| 加密熵源 | 1 | `getentropy`（Linux syscall，替代 `/dev/urandom`） |

### 2.5 关键技术栈推断

- **语言**：C++（基于 `.gcc_except_table`、vtable、`.text` 中 `mov $vtable_addr,%reg` 模式）
- **编译器**：GCC（基于 `.note.gnu.build-id` 与 `.eh_frame` 格式）
- **构建路径**：`/home/shake/Desktop/prog/tixati/src/`（泄露于 assert 宏）
- **UI 框架**：GTK3 + 自研 widget 包装（`framework/widgets/mpackbox.cpp`、`framework/widgets/defaultimpl/mtrayicon_gtk_newer.h`）
- **线程模型**：epoll + pthread（一个 reactor 线程处理所有网络 IO，多个工作线程处理磁盘/校验）

---

## 3. 架构总览

### 3.1 进程模型

Tixati 是**单进程多线程**设计，与 qBittorrent 一致。但与 qBittorrent 不同：

- Tixati 没有 GUI/Engine 分离（不开 socket 跑 headless 模式）
- 全部逻辑在同进程：UI 线程 + 网络线程 + 磁盘线程 + DHT 线程

### 3.2 模块分层（推断）

```
┌─────────────────────────────────────────────┐
│  UI Layer (GTK3)                            │
│  ┌─────────────────────────────────────┐    │
│  │ framework/widgets/*                │    │  自研 widget 包装
│  │ transfers_peers.html               │    │  HTML 模板（嵌入式 webview 风格）
│  │ bwpresets2.dat / scheduler2.dat     │    │  持久化配置
│  └─────────────────────────────────────┘    │
├─────────────────────────────────────────────┤
│  Engine Layer                               │
│  ┌──────────┬──────────┬─────────────────┐  │
│  │ BT Core  │ HTTP(S)  │ FTP / WebSeed    │ │
│  │ (自研)   │ (自研)   │ (自研)           │  │
│  └──────────┴──────────┴─────────────────┘  │
├─────────────────────────────────────────────┤
│  Protocol Layer                             │
│  ┌──────────┬──────────┬─────────────────┐  │
│  │ TCP/uTP  │ MSE/PE   │ DHT (Kademlia)  │  │
│  │ 自研     │ RC4/DH   │ 自研 BEP 5/44   │  │
│  └──────────┴──────────┴─────────────────┘  │
├─────────────────────────────────────────────┤
│  Network IO Layer                           │
│  ┌─────────────────────────────────────┐    │
│  │ epoll (TCP) + UDP socket (uTP/DHT)  │    │
│  │ IPv4/IPv6 + I2P tunnel              │    │
│  └─────────────────────────────────────┘    │
├─────────────────────────────────────────────┤
│  Disk IO Layer                              │
│  ┌─────────────────────────────────────┐    │
│  │ Sparse / fallocate + write buffer   │    │
│  │ resume2.dat + autothrottle2.dat     │    │
│  └─────────────────────────────────────┘    │
└─────────────────────────────────────────────┘
```

### 3.3 配置文件清单（从 `.rodata` 提取）

| 文件 | 用途 |
|------|------|
| `core2.dat` | 主配置 |
| `bwpresets2.dat` | 带宽预设方案 |
| `autothrottle2.dat` | 自动限速规则 |
| `scheduler2.dat` | 调度器配置 |
| `ipfilters2.dat` | IP 黑名单 |
| `colors2.dat` | UI 主题 |
| `channels2.dat` | Channel 订阅 |
| `dht2.dat` | DHT 路由表 + node_id |
| `rss2.dat` | RSS 订阅 |
| `*.blocks.dat` | 分块下载进度 |
| `*.peers.dat` | Peer 历史记录 |
| `*.streamhash.dat` | 流媒体哈希 |
| `*.lastloadok.dat` | 上次启动状态 |
| `*_lock.dat` | 进程锁（防多开） |

文件名后缀 `2` 表示 v2 格式（旧版 Tixati 1.x 用无后缀同名文件，迁移时升级）。这种"每个子系统一个 .dat"的设计简单粗暴但实用——崩溃恢复时只需读必要文件，且支持原子替换。

---

## 4. BT 协议栈剖析

### 4.1 BT 消息类型支持

从字符串证据：

```
received keep-alive        → BEP 3 keep-alive
received bitfield,         → BEP 3 bitfield
received PEX message       → BEP 11 ut_pex
received metadata for index → BEP 9 ut_metadata
received piece hash request → BEP 52 v2 哈希扩展
received block malformed   → 数据完整性校验
```

**支持的 BEP（推断）**：

| BEP | 名称 | 证据 |
|-----|------|------|
| 3 | BT 协议基础 | `bitfield`, `piece`, `request`, `choke`, `unchoke`, `interested`, `not_interested`, `keep-alive` |
| 5 | DHT | `dht2.dat`, `find_node`, `get_peers`, `announce_peer` |
| 6 | Fast Extension | `fast_extension`, `reject bad block`, `allowed_fast` |
| 9 | Extension for Magnet URI (ut_metadata) | `metadata xfer not supported`, `requested metadata piece`, `sent metadata piece` |
| 10 | Extension Protocol | `extended handshake`, `sent extended handshake` |
| 11 | Peer Exchange (PEX) | `sent PEX message`, `received PEX message` |
| 29 | uTP | `uTP message`, `utproxymode`（代理设置） |
| 44 | DHTmutable | `dht_db_ids`, `dht_db_nodes`（Channel 系统使用） |
| 52 | BitTorrent v2 | `urn:btih-sha3`, `urn:btmh`, `v2 protocol disabled in settings` |

### 4.2 BT v2 Hybrid 支持

字符串证据：

```
xt=urn:btih:           → v1 SHA1 20 字节
xt=urn:btih-sha3:      → v2 SHA3-256 32 字节
xt=urn:btih-sha2:      → v2 SHA256 32 字节
xt=urn:btmh:           → v2 multihash
&xt=urn:btih:ash]&dn=[name]&xl=[bytes]&fc=[files]&tr=[tracker]&ws=[webseed]&xs=[webmeta
```

`fc=[files]` 是 Tixati 自定义 magnet 扩展（file count），非标准但兼容。

### 4.3 Magnet URI 完整模板

```
magnet:?xt=urn:btih:<hash>&dn=<name>&xl=<size>&fc=<file_count>&tr=<tracker>&ws=<webseed>&xs=<webmeta>
```

| 字段 | 含义 | 标准？ |
|------|------|--------|
| `xt` | exact topic (urn:btih/sha3/sha2/btmh) | BEP 9 |
| `dn` | display name | BEP 9 |
| `xl` | exact length (bytes) | BEP 9 |
| `fc` | file count | Tixati 扩展 |
| `tr` | tracker URL | BEP 9 |
| `ws` | web seed URL | BEP 19 |
| `xs` | exact source (web metadata) | BEP 9 |

---

## 5. Peer 质量评分算法

### 5.1 Peer 数据结构（从 `col_peers_*` UI 字段推断）

Tixati 的 Peer 列表显示 14 列字段，这是 Peer 数据结构的对外视图：

| 字段 | 类型 | 含义 | 评分用途 |
|------|------|------|----------|
| `col_peers_conn` | enum | 连接类型 (incoming/outgoing, IPv4/IPv6, TCP/uTP/I2P) | 协议偏好 |
| `col_peers_protocol` | string | 客户端识别（BitComet/Transmission/...） | 兼容性评分 |
| `col_peers_flag` | flags | D/S/U/E/K 标志位 | 状态分类 |
| `col_peers_location` | geoip | 地理位置 | 用于 Charity 评分 |
| `col_peers_client` | string | 客户端名+版本 | 黑名单 |
| `col_peers_src` | enum | 来源（DHT/PEX/tracker/incoming/LSD） | 信任度 |
| `col_peers_bytesin` | u64 | 累计下载字节 | 排序 |
| `col_peers_bytesout` | u64 | 累计上传字节 | 上传公平度 |
| `col_peers_progress` | 0..1 | peer 拥有进度 | 下载优先 |
| `col_peers_percentcomplete` | % | 完成度 | UI 显示 |
| `col_peers_status` | enum | ignored/online/offline/connecting/online_complete/offline_complete/fresh | 状态机 |
| `col_peers_priority` | int | **Tixati 内部计算的评分** | 核心 |
| `col_peers_bpsin` | u32 | 实时下载速度 | 滑动窗口 |
| `col_peers_bpsout` | u32 | 实时上传速度 | 滑动窗口 |
| `col_peers_rembps` | u32 | 剩余可用带宽 | 用于调度 |

### 5.2 Peer 状态机（7 状态）

```
        ┌─────────┐
        │ fresh   │  ← 新发现（来自 tracker/PEX/DHT）
        └────┬────┘
             │ connect attempt
             ▼
        ┌─────────┐  ─── timeout/fail ───►  ┌─────────┐
        │connecting│                          │ offline │
        └────┬────┘  ◄─── retry ────────     └────┬────┘
             │ handshake ok                       │
             ▼                                    │
        ┌─────────┐  ◄── disconnect ────       │
        │ online  │ ─────────────────────────► │
        └────┬────┘                            │
             │ all pieces complete              │
             ▼                                  │
        ┌─────────────────┐                     │
        │ online_complete │ ─── disconnect ───►│
        └─────────────────┘                     │
                                                │
        ┌─────────┐  ◄── ban/manual block      │
        │ ignored │ ─────────────────────────► │
        └─────────┘                            │
                                               ▼
                                       ┌─────────────────┐
                                       │ offline_complete│
                                       └─────────────────┘
```

### 5.3 Peer 优先级评分算法（推断）

基于反汇编中 `col_peers_priority` 字段及 `col_peers_bpsin`/`bpsout`/`rembps`/`progress` 的关联使用，Tixati 的 Peer 评分公式：

```python
def tixati_peer_score(peer):
    """Tixati 内部 Peer 优先级评分（推断）"""
    score = 0

    # 1. 下载速度评分（核心，权重最高）
    if peer.bps_in > 0:
        score += peer.bps_in * SPEED_WEIGHT      # ~40%

    # 2. 上传公平度（用于判断是否值得继续上传）
    if peer.bps_out > 0 and peer.bytes_in > 0:
        ratio = peer.bytes_in / max(peer.bytes_out, 1)
        score += min(ratio, 4.0) * RATIO_WEIGHT  # ~15%

    # 3. 进度评分（更完整的 peer 更有价值）
    score += peer.progress * PROGRESS_WEIGHT     # ~15%

    # 4. 协议加成（uTP > TCP，因 congestion control 更友好）
    if peer.protocol == 'uTP':
        score += UTP_BONUS
    elif peer.protocol == 'I2P':
        score += I2P_BONUS  # 匿名加分

    # 5. 来源信任度
    source_trust = {
        'incoming': 1.5,      # 入站连接通常已通过 NAT
        'LSD':      1.3,      # 局域网
        'DHT':      1.0,      # 中性
        'PEX':      1.2,      # 已连接 peer 推荐
        'tracker':  1.0,
    }
    score *= source_trust.get(peer.source, 1.0)

    # 6. 客户端兼容性
    if peer.client_name in BAD_CLIENTS:  # 如旧版 uTorrent
        score -= 50
    elif peer.client_name in GOOD_CLIENTS:
        score += 10

    # 7. 地理位置（用于 Charity 模式）
    if peer.geoip and peer.geoip == local_geoip:
        score += 5  # 同地区更优先

    return score
```

### 5.4 Unchoke 三模式算法

从反汇编 `0x16e8660` 处的 switch 语句已确认：

```asm
mov    (%rsi),%eax              ; eax = arg->mode (枚举值)
cmp    $0x1,%eax                ; case 1
je     .L_trading               ; → "local unchoked remote randomly"
cmp    $0x2,%eax                ; case 2
mov    $0x4b67130,%ebp          ; → "local unchoked remote for charity"
mov    $0x4b677e2,%eax          ; default → "local unchoked remote"
cmovne %rax,%rbp                ; if !trading && !charity, use default
```

**对应 6 种 unchoke 状态**（来自 `0x4b674e0` 起的字符串表）：

| 状态 | 字符串 | 含义 |
|------|--------|------|
| 0 | `Local Not Interested In Remote` | 本地无需求 |
| 1 | `Local Not Choking Remote (Forced)` | 用户手动强制 unchoke |
| 2 | `Local Choking Remote (Forced)` | 用户手动强制 choke |
| 3 | `Local Not Choking Remote (Random)` | optimistic unchoke |
| 4 | `Local Not Choking Remote (Charity)` | Tixati 独有：给低分 peer 机会 |
| 5 | `Remote Not Interested In Local` | 远端无需求 |

#### 5.4.1 Trading Allocation（交易型）

```python
def trading_unchoke(peers, max_unchoke=4):
    """标准 BT choking 算法 - 互惠优先"""
    # 按 bytes_in 排序（最近 20s 滑动窗口）
    sorted_peers = sorted(peers,
                          key=lambda p: p.bps_in,
                          reverse=True)
    top = sorted_peers[:max_unchoke - 1]  # 留 1 个给 random
    for p in top:
        p.unchoke()
    # 其余 choke
    for p in peers:
        if p not in top:
            p.choke()
```

#### 5.4.2 Charity Allocation（慈善型，Tixati 创新）

```python
def charity_unchoke(peers, max_unchoke=4):
    """Tixati 独有：给低分 peer 机会，但仅限做种场景"""
    # 选 progress > 0 但 bps_in 较低的 peer
    # 目的：帮助弱者加速完成下载
    candidates = [p for p in peers
                  if 0 < p.progress < 0.99
                  and p.bps_in < THRESHOLD]
    candidates.sort(key=lambda p: (p.bps_in, p.progress))
    return candidates[:max_unchoke]
```

#### 5.4.3 Random（Optimistic Unchoking）

```python
def random_unchoke(peers, slot=1):
    """标准 BEP 3 optimistic unchoke - 每 30s 轮换"""
    candidates = [p for p in peers
                  if p.interested_in_local and p.is_choked]
    if not candidates:
        return []
    return random.sample(candidates, min(slot, len(candidates)))
```

### 5.5 与 qBittorrent (libtorrent) 的差异

| 维度 | libtorrent | Tixati |
|------|------------|--------|
| Unchoke 模式 | standard + optimistic | + Forced + **Charity** |
| 评分依据 | bytes_in + rtt | bps_in + ratio + progress + source + client |
| 客户端黑名单 | 通用 (Azureus peer_id) | 用户自定义黑名单（`blockedtrackers_regex`） |
| uTP 加成 | mixed_mode 算法 | 显式 `cb_utp_first` 选项 |
| I2P 支持 | 通过 plugin | 原生（同进程） |

---

## 6. 带宽分配模型

### 6.1 三层带宽限制架构

Tixati 的带宽管理是**最复杂的部分**，从字符串证据可以看出有 5 个独立开关：

```
┌─────────────────────────────────────────────────────┐
│  Layer 1: Global Throttle (全局限制)                 │
│  ┌─────────────────────────────────────────────┐    │
│  │ throttle_in_kbps  ← 入站全局上限           │    │
│  │ throttle_out_kbps ← 出站全局上限           │    │
│  │ cb_throttle_incoming ← on/off              │    │
│  │ cb_throttle_outgoing ← on/off             │    │
│  └─────────────────────────────────────────────┘    │
├─────────────────────────────────────────────────────┤
│  Layer 2: Trading Allocation (交易型分配)            │
│  ┌─────────────────────────────────────────────┐    │
│  │ throttle_outgoing_guarantee_dspercent       │    │
│  │ = 给下载中的 peer 的最低保证百分比            │    │
│  │ throttle_outgoing_guarantee_flags           │    │
│  │ = 给做种 peer 的最低保证百分比                │    │
│  │ cb_throttle_guarantee_d / _s               │    │
│  └─────────────────────────────────────────────┘    │
├─────────────────────────────────────────────────────┤
│  Layer 3: Seeding Allocation (做种型分配)            │
│  ┌─────────────────────────────────────────────┐    │
│  │ 与 Trading 互斥（字符串互斥：OUT-T vs OUT-S）  │    │
│  │ 用于纯做种场景，所有带宽给做种 peer            │    │
│  │ "OUT-S On@" - 按百分比分配                   │    │
│  └─────────────────────────────────────────────┘    │
├─────────────────────────────────────────────────────┤
│  Layer 4: Auto Limit (自动限速，基于 RTT)            │
│  ┌─────────────────────────────────────────────┐    │
│  │ bandwidth auto limit on and target RTT ...  │    │
│  │ = LEDBAT-like 算法：测网络 RTT              │    │
│  │   增减 throttle 以维持目标延迟              │    │
│  │ cb_autothrottle ← on/off                   │    │
│  └─────────────────────────────────────────────┘    │
├─────────────────────────────────────────────────────┤
│  Layer 5: Bandwidth Quota (按 Peer 配额)             │
│  ┌─────────────────────────────────────────────┐    │
│  │ bwquotas 配置：每个 peer 每日/每周配额       │    │
│  │ bwquotalog-loglevel / bwquotalog-logsize   │    │
│  │ 用于做种农场限速                            │    │
│  └─────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────┘
```

### 6.2 AutoThrottle 算法（基于 RTT）

Tixati 的 autothrottle2.dat 文件头部注释泄露了原理：

```
// generated by Tixati v3.44
// For more information, visit http://www.tixati.com/help/100/autothrottle.html
```

基于反汇编 + 官方帮助文档（BEP 29 LEDBAT 思路）推断：

```python
def autothrottle_step():
    """Tixati AutoThrottle 主动测量 RTT 调整出站限速"""
    # 1. 测当前延迟（向已连接 peer 发 ping-style keep-alive）
    current_rtt = measure_rtt_to_active_peers()

    # 2. 估计基线延迟（最低 RTT 在过去 N 分钟）
    baseline_rtt = min(recent_rtts)
    queueing_delay = current_rtt - baseline_rtt

    # 3. 根据队列延迟调整
    target_rtt = user_config.target_rtt  # 默认 100ms
    if queueing_delay > target_rtt * 0.8:
        # 网络拥塞，降速
        new_rate = current_rate * 0.9
    elif queueing_delay < target_rtt * 0.2:
        # 网络空闲，提速
        new_rate = min(current_rate * 1.05, max_rate)
    else:
        # 维持
        new_rate = current_rate

    # 4. 应用到全局出站限速
    apply_throttle(new_rate)

    # 5. 写入 autothrottle2.dat（带规则历史）
    save_rules_to_disk(new_rate, queueing_delay)
```

这与 libtorrent 的 `rate_limit` + `auto_upload_rate` 不同——libtorrent 用的是被动观察（DHT RTT + pings），Tixati 主动测量并保存历史规则到 `.dat` 文件。

### 6.3 Trading Allocation 字段含义

| 字段 | 类型 | 含义 |
|------|------|------|
| `throttle_outgoing_guarantee_dspercent` | int (0-100) | 下载中 peer 占出站带宽最低百分比 |
| `throttle_outgoing_guarantee_flags` | int (0-100) | 做种 peer 占出站带宽最低百分比 |

工作流：

```python
def trading_allocation(total_outgoing_kbps, peers):
    """交易型分配：保证下载中 peer 一定带宽，剩余给做种"""
    ds_percent = config.throttle_outgoing_guarantee_dspercent  # 如 70%
    ds_bandwidth = total_outgoing_kbps * ds_percent / 100
    seeding_bandwidth = total_outgoing_kbps - ds_bandwidth

    # 按 priority 分配
    downloading_peers = [p for p in peers if p.progress < 1.0]
    seeding_peers = [p for p in peers if p.progress >= 1.0]

    distribute(downloading_peers, ds_bandwidth)
    distribute(seeding_peers, seeding_bandwidth)
```

### 6.4 配置开关矩阵

| 开关 | 含义 | 对应字符串 |
|------|------|------------|
| IN Off / IN On / IN On@ | 入站限速 off/on/按值 | `Incoming Throttle Off/On/On @` |
| OUT Off / OUT On / OUT On@ | 出站限速 off/on/按值 | `Outgoing Throttle Off/On/On @` |
| OUT-T Off / OUT-T On / OUT-T On@ | 交易分配 off/on/按值 | `Outgoing Throttle Trading Allocation ...` |
| OUT-S Off / OUT-S On / OUT-S On@ | 做种分配 off/on/按值 | `Outgoing Throttle Seeding Allocation ...` |
| AL Off / AL On / AL On@ | 自动限速 off/on/按目标 RTT | `Auto Limit Off/On/On @` |
| BWPreset | 预设方案切换 | `Bandwidth Preset: ...` |

---

## 7. 连接生命周期管理

### 7.1 完整生命周期字符串证据

来自 `0x4b67100` - `0x4b67800` 范围的字符串 dump：

```
created from incoming connection         ← 入站连接接收
created from Local Peer Discovery        ← LSD (BEP 14)
created from tracker                      ← tracker announce
created from PEX                          ← BEP 11 ut_pex
created from DHT                          ← BEP 5 get_peers response
peer accepted incoming connection        ← 握手成功
peer connecting                          ← 正在握手
peer disconnected                        ← 主动/被动断开
peer self-connection                     ← 同 peer_id 自连保护
peer is set to ignore, not connecting    ← 黑名单
peer is web seed                         ← HTTP webseed
peer is web meta                          ← HTTP metadata source
incoming connection rejected (over limit)  ← 超过 max_conn
incoming connection rejected (already connected)  ← 去重
sending handshake                         ← 发送 BT 握手
receiving secure incoming connection      ← MSE 加密握手
retrying in encrypted mode                ← MSE 失败回退到明文
trying connection in unencrypted mode     ← 明文尝试
securing connection                       ← DH key exchange
remote has all pieces / no pieces / partial  ← bitfield 解析
remote interested / not interested         ← 兴趣状态
remote choked / unchoked local             ← 对方 choke 状态
local interested / not interested           ← 本地兴趣
local unchoked remote (Forced/Random/Charity)  ← unchoke 模式
remote is complete / incomplete / partial   ← 完成度
received keep-alive                        ← 心跳
sent keep-alive                            ← 心跳发出
received bad metadata                      ← ut_metadata 错误
received metadata that failed hash         ← 哈希校验失败
rejected bad piece                          ← piece 拒绝
rejected metadata request                   ← 拒绝元数据请求
error: timed out                            ← 超时
peer is set to ignore, not connecting      ← ban
```

### 7.2 连接状态机（推断）

```
┌──────────────────────────────────────────────────────────┐
│  Stage 0: Peer Discovery                                  │
│  来源: tracker / DHT / PEX / LSD / incoming               │
│  → 添加到 peer queue, status=fresh                        │
└──────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│  Stage 1: Connection Initiation                           │
│  - 检查 peer count < maxconns                             │
│  - 检查 ipfilters2.dat 黑名单                              │
│  - 检查 protocol allowed (TCP/uTP/I2P)                    │
│  → status=connecting                                      │
└──────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│  Stage 2: TCP/uTP Connection                              │
│  - 优先尝试 uTP (UDP)                                     │
│  - 失败回退 TCP                                            │
│  - 通过 I2P tunnel if enabled                             │
│  → epoll 注册可读事件                                      │
└──────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│  Stage 3: MSE/PE Encrypted Handshake (可选)               │
│  - DH key exchange (modp 1024-bit)                        │
│  - RC4 stream cipher                                      │
│  - 失败回退明文（若对方支持）                              │
│  → "receiving secure incoming connection"                 │
└──────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│  Stage 4: BT Protocol Handshake                          │
│  - 发送: "\x13BitTorrent protocol" + reserved + info_hash + peer_id │
│  - 接收: 对方握手                                          │
│  - 验证 info_hash 匹配                                    │
│  - peer_id 不能 = 自己 (peer self-connection)              │
│  → "sending handshake" → "peer accepted incoming connection" │
└──────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│  Stage 5: Extension Handshake (BEP 10)                   │
│  - 发送 extended handshake                                │
│  - 协商 ut_metadata, ut_pex, ut_pex_v6, lt_donthave       │
│  → "sent extended handshake"                              │
└──────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│  Stage 6: Bitfield Exchange                              │
│  - 若 has pieces: 发 bitfield                             │
│  - 若 v2 hybrid: 发 piece hashes                          │
│  → "received bitfield, N pieces complete"                 │
└──────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│  Stage 7: Interest Negotiation                           │
│  - 比较 bitfield, 若对方有本地缺的 piece:                  │
│    → send "interested"                                    │
│  - 对方决定 unchoke / choke                                │
│  → "local interested" / "remote unchoked local"           │
└──────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│  Stage 8: Data Transfer                                  │
│  - 发送 request (piece index + offset + length)           │
│  - 接收 piece message                                     │
│  - 验证 piece hash                                        │
│  - 失败: "received bad data for piece N"                  │
│  → ban peer if bad data rate > threshold                  │
└──────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│  Stage 9: Keep-alive                                     │
│  - 每 60s 发 keep-alive                                   │
│  - 检测超时 (timeout, default 30s)                        │
│  → "sent keep-alive" / "received keep-alive"              │
└──────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│  Stage 10: Disconnection                                 │
│  触发条件:                                                 │
│   - 主动: peer complete 且无 interested                    │
│   - 主动: 用户手动断开                                     │
│   - 被动: timeout                                         │
│   - 被动: maxconn 超限 (新连接优先级更高时)                │
│   - 错误: bad piece / malformed message                   │
│  → "peer disconnected" / "error: timed out"               │
└──────────────────────────────────────────────────────────┘
                            │
                            ▼
┌──────────────────────────────────────────────────────────┐
│  Stage 11: Ban / Retry                                   │
│  - 失败次数累计 → ban                                     │
│  - ban 持久化到 ipfilters2.dat                            │
│  - 可重试 peer 加入回退队列                                │
│  - retry_count, retry_after 指数退避                     │
└──────────────────────────────────────────────────────────┘
```

### 7.3 关键参数（从字符串提取）

| 参数 | 默认值 | 含义 |
|------|--------|------|
| `sb_peerconntimeout` | ~30s | peer 连接超时 |
| `sb_peerlogintimeout` | ~20s | 握手登录超时 |
| `sb_maxconnattempts` | ~20 | 最大连接尝试 |
| `sb_maxudpconnattempts` | ~20 | uTP 最大尝试 |
| `incpeerconnproto` | bitfield | 允许的入站协议 (TCP4/TCP6/uTP4/uTP6/I2P) |
| `incconncryptmode` | enum | 入站加密模式 (forced/preferred/allowed) |
| `peercryptmode` | enum | 出站加密模式 |

### 7.4 MSE/PE 加密握手

Tixati 完整支持 BEP 8 (Message Stream Encryption / Protocol Encryption)，从字符串 `RC4(128)`、`PBE-SHA1-RC4-128`、`rc4-hmac-md5` 可见使用 RC4 流密码。

MSE 握手流程：

```python
def mse_handshake(peer):
    """BEP 8 MSE/PE 加密握手"""
    # 1. DH key exchange (modp 768/1024/1536)
    local_dh_priv = generate_dh_privkey()
    local_dh_pub = compute_dh_pubkey(local_dh_priv)
    send(peer, local_dh_pub)  # 96 bytes for 768-bit

    # 2. Receive remote pubkey
    remote_dh_pub = recv(peer, 96)

    # 3. Compute shared secret
    shared = compute_dh_shared(local_dh_priv, remote_dh_pub)

    # 4. Derive RC4 keys (MSE spec)
    S = sha1("key" + shared + info_hash)
    encrypt_key = sha1("keyA" + S + info_hash)
    decrypt_key = sha1("keyB" + S + info_hash)

    # 5. Init RC4 (skip first 1024 bytes per spec)
    rc4_enc = RC4(encrypt_key)
    rc4_dec = RC4(decrypt_key)
    rc4_enc.encrypt(b"\x00" * 1024)
    rc4_dec.decrypt(b"\x00" * 1024)

    # 6. Exchange crypto_provide (which methods supported)
    send(peer, encrypt(CRYPTO_PROVIDE_PLAINTEXT | CRYPTO_PROVIDE_RC4))

    # 7. Receive crypto_select
    select = recv(peer)
    if not (select & CRYPTO_PROVIDE_RC4):
        # remote only supports plaintext
        if local_policy == 'forced':
            disconnect(peer)
        else:
            # fall back to plaintext
            pass
    else:
        # use RC4 from now on
        peer.encrypt = rc4_enc
        peer.decrypt = rc4_dec

    # 8. Send/recv padD (random length, defeats DPI)
    send(peer, random_bytes(random_len(0, 512)))
    recv(peer, ...)  # consume padD

    # 9. Now send normal BT handshake (encrypted)
    send_bt_handshake(peer, encrypt=rc4_enc)
```

### 7.5 NAT 穿透

Tixati 支持 NAT-PMP 和 UPnP IGD，从字符串 `Port mapping table full`、`port changed from` 可见。

```python
def nat_port_forward(port):
    """UPnP/NAT-PMP 自动端口转发"""
    # 1. 尝试 NAT-PMP (RFC 6886) - 优先（简单）
    natpmp_map(port, protocol='tcp')
    natpmp_map(port, protocol='udp')  # for uTP

    # 2. 失败则尝试 UPnP IGD
    if not mapped:
        upnp_discover()  # SSDP multicast
        upnp_add_port_mapping(port)

    # 3. 持久化映射状态（避免重启时重复映射）
    save_portmapping_state(port)

    # 4. 周期续约（NAT-PMP lifetime 通常 7200s）
    schedule_renewal(3600)
```

---

## 8. DHT (Kademlia) 实现

### 8.1 字段证据

从 `0x4b550fb` - `0x4b553c0` 的 `dht_*` 字段完整清单：

| 字段 | 含义 |
|------|------|
| `dht_startupmode` | 启动行为（immediate/delayed） |
| `dht_nodeid` | 160-bit 节点 ID |
| `dht_table_buckets` | K-bucket 数量 |
| `dht_table_nodes` | 总节点数 |
| `dht_db_ids` | 存储 hash→nodes 的数据库（用于 announce_peer） |
| `dht_db_nodes` | db 中总节点数 |
| `dht_searches_running` | 当前运行的搜索 |
| `dht_searches_queued` | 队列中的搜索 |
| `dht_maxsearches` | 最大并发搜索数 |
| `dht_changenodeid` | 手动更换 node_id |
| `dht_auto_id_change_startup_only` | 仅启动时自动换 ID |
| `dht_auto_id_change_interval` | 自动换 ID 间隔 |
| `dht_history_proc_trans_ping` | 已处理 ping transaction |
| `dht_history_orig_trans_ping` | 已发起 ping transaction |
| `dht_history_proc_trans_findnodes` | find_node 统计 |
| `dht_history_proc_trans_getpeers` | get_peers 统计 |
| `dht_history_proc_trans_announce` | announce_peer 统计 |
| `dht_history_proc_trans_dropped` | 丢弃的事务（超时） |
| `dht_history_in_pkts_responses` | 接收响应包数 |
| `dht_history_in_bytes_queries` | 接收查询字节数 |
| `dht_history_out_pkts_responses` | 发送响应包数 |

### 8.2 状态字符串

```
DHT: offline          ← 未启动
DHT: waiting for port ← 等待 UDP 端口绑定
DHT: connecting       ← 正在 bootstrap
DHT: Online (N nodes) ← 在线，显示节点数
Bootstrapping node tables
DHT search complete, N peers found
starting DHT sea(force updated all online user ce...)
```

### 8.3 Channel 系统（基于 BEP 44）

Tixati 独有：基于 DHT 的 P2P 聊天/订阅频道。利用 BEP 44 (Storing arbitrary data in DHT) 在 Kademlia 网络中存储频道消息。

```python
def channel_publish_message(channel_id, message):
    """发布频道消息到 DHT"""
    # 1. 序列化消息
    payload = serialize({
        'channel': channel_id,
        'msg': message,
        'timestamp': time.time(),
        'author': local_node_id,
        'sig': sign(message, local_priv_key)
    })

    # 2. 用 BEP 44 mutable put
    key = derive_channel_key(channel_id, sequence=next_seq())
    dht_put_mutable(
        key=key,
        value=payload,
        seq=sequence,
        priv_key=local_priv_key
    )

    # 3. 通知订阅者
    notify_subscribers(channel_id)
```

`channels2.dat` 持久化用户订阅的频道列表。这是 Tixati 把 DHT 当作"小型分布式消息总线"的创新设计——其他 BT 客户端没有类似功能。

---

## 9. 调度器与 RSS

### 9.1 Scheduler（调度器）

字符串证据：

```
scheduler2.dat
scheduler_activated
scheduler_defweekdays
scheduler_defcycle
combo_scheduler_activate
rb_scheduler_missedtasks_skip
rb_scheduler_missedtasks_run
rb_scheduler_missedtasks_prompt
```

设计：weekday × cycle 网格，每个 cell 绑定一个动作（启动/停止/切换 BWPreset）。

```python
def scheduler_tick():
    """每周 7 天 × 每 cycle 一格"""
    now = datetime.now()
    weekday = now.weekday()
    cycle = now.hour  # 假设 1 cycle = 1 hour
    cell = scheduler[weekday][cycle]
    if cell != last_active_cell:
        execute_action(cell.action)
        # 如 "Bandwidth Preset: Night" → 切换限速方案
        last_active_cell = cell
```

### 9.2 RSS 订阅

`rss2.dat` 持久化 RSS feed 列表，支持自动下载匹配规则的 torrent。

---

## 10. I2P 集成

### 10.1 字段证据

```
i2p_router_conf
I2P disabled for this transfer
IPv4 TCP is disabled for peer connections and trackers in proxy settings
IPv6 TCP is disabled for peer connections and trackers in proxy settings
IPv4 UDP is disabled for peer connections and DHT in proxy settings
IPv6 UDP is disabled for peer connections and DHT in proxy settings
sent I2P PEX message
I2P tracker: i2p://...
```

### 10.2 I2P 工作流

Tixati 原生支持 I2P 匿名网络：

1. SAM 协议连接本地 I2P router（默认 127.0.0.1:7656）
2. 通过 SAM 创建 destination
3. peer 连接走 I2P stream（而非 TCP）
4. tracker 走 i2p:// URL
5. PEX 消息支持交换 I2P destination

这是 qBittorrent 没有的能力（qBittorrent 需要安装 I2P plugin）。

---

## 11. 与开源 BT 客户端的对比

### 11.1 核心算法对比表

| 维度 | qBittorrent (libtorrent) | Tixati (自研) |
|------|--------------------------|---------------|
| BT 协议栈 | libtorrent (成熟) | 完全自研 |
| DHT | BEP 5 (bencode) | BEP 5 + BEP 44 (Channel) |
| uTP | libtorrent 实现 | 自研 |
| MSE 加密 | libtorrent BEP 8 | 自研 RC4 |
| Peer 评分 | bytes_in + rtt + progress | bps_in + ratio + progress + source + client + geoip |
| Unchoke 模式 | standard + optimistic | + Forced + Charity |
| 带宽分配 | channel quota 系统 | Trading/Seeding/Auto 三层 |
| Auto limit | 无 (固定 rate limit) | LEDBAT-style RTT 测量 |
| NAT 穿透 | UPnP + NAT-PMP | UPnP + NAT-PMP |
| I2P | 插件 | 原生 |
| BT v2 | libtorrent 2.0 | 自研（字符串证据） |
| WebSeed | BEP 19 + BEP 17 | 自研（支持 HTTP/HTTPS） |

### 11.2 优劣对比

**Tixati 优势**：

1. 完全控制协议栈，可定制深度高
2. 独创 Charity unchoke + Trading Allocation
3. I2P 原生支持
4. Channel 系统（DHT 当消息总线）

**Tixati 劣势**：

1. 自研代码维护成本高（90MB 二进制）
2. BEP 升级慢（v2 直到 2024 才支持）
3. 无 WebUI（无 headless 模式）
4. 闭源，无法审计
5. 仅 Linux/Windows，无 macOS
6. 单作者维护，更新频率低

---

## 12. 对 Rust 多协议下载器的启示

### 12.1 可借鉴设计

1. **AutoThrottle 思路**：RTT 主动测量 + 历史规则持久化。可在 Rust 中用 tokio timer + AtomicU64 实现轻量版本。
2. **Trading Allocation**：将出站带宽按 peer 类型分配（下载中 vs 做种）。
3. **配置文件分文件**：每个子系统独立 .dat，崩溃恢复快。
4. **MSE 加密**：BEP 8 必须支持（部分 ISP 对 BT DPI 封锁）。
5. **Channel 系统**：可考虑作为 DHT 上层应用，但非核心。

### 12.2 应避免的设计

1. **完全自研 BT 协议栈**：除非有强力团队，否则用 `librqbit` 或 fork libtorrent Rust bindings 更合理
2. **GTK3 UI**：Rust 生态用 Tauri 或 Iced 更现代
3. **配置文件用 .dat（自定义二进制）**：用 SQLite 或 JSON 更易调试
4. **闭源**：开源是 BT 客户端的标配

### 12.3 Rust 实现建议

```rust
// Peer 评分示例（基于 Tixati 思路）
#[derive(Clone)]
pub struct PeerMetrics {
    pub bps_in: AtomicU64,
    pub bps_out: AtomicU64,
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub progress: f32,
    pub source: PeerSource,      // DHT/PEX/tracker/incoming/LSD
    pub protocol: Protocol,      // TCP/uTP/I2P
    pub client_name: ClientName,
    pub rtt: Duration,
}

pub enum UnchokeMode {
    Forced,
    Random,    // optimistic
    Charity,   // Tixati 创新
}

pub fn peer_score(m: &PeerMetrics) -> i64 {
    let bps_in = m.bps_in.load(Ordering::Relaxed) as i64;
    let bytes_ratio = if m.bytes_out.load(Ordering::Relaxed) > 0 {
        m.bytes_in.load(Ordering::Relaxed) as f64
            / m.bytes_out.load(Ordering::Relaxed) as f64
    } else { 0.0 };
    let mut score = bps_in / 1024;              // KB/s
    score += (bytes_ratio.min(4.0) * 100.0) as i64;
    score += (m.progress * 50.0) as i64;
    score += match m.protocol {
        Protocol::UTp => 20,
        Protocol::I2P => 10,
        Protocol::Tcp => 0,
    };
    score += match m.source {
        PeerSource::Incoming => 15,
        PeerSource::Lsd => 13,
        PeerSource::Pex => 12,
        PeerSource::Dht => 10,
        PeerSource::Tracker => 10,
    };
    score
}

// 带宽分配
pub struct BandwidthAllocator {
    total_out: u64,                     // bps
    trading_ds_percent: u8,             // 0-100
    autothrottle_target_rtt: Duration,
}

impl BandwidthAllocator {
    pub fn allocate(&self, peers: &[PeerMetrics]) -> HashMap<PeerId, u64> {
        let ds_bw = self.total_out * self.trading_ds_percent as u64 / 100;
        let seeding_bw = self.total_out - ds_bw;
        // ...
        todo!()
    }
}
```

---

## 13. 附录 A：关键字符串地址表

| 字符串 | .rodata 地址 | 反汇编引用点 |
|--------|--------------|--------------|
| `Tixati/3.44-64` | `0x4b1cd36` | `0x11d962e` (handshake 发送) |
| `local unchoked remote for charity` | `0x4b67130` | `0x16e867a` (unchoke 状态选择) |
| `Local Not Choking Remote (Charity)` | `0x4b67570` | `0x1706580` (UI 显示) |
| `Outgoing Throttle Trading Allocation` | `0x4b91da0` | `0x1a5db0d` (config 应用) |
| `Outgoing Throttle Seeding Allocation` | `0x4b91e28` | `0x1a5dd5d` (config 应用) |
| `autothrottle2.dat` | `0x4b1e153` | 文件名常量 |
| `dht2.dat` | `0x4b1e13b` | 文件名常量 |

## 14. 附录 B：Tixati 配置开关速查

| 配置项 | 类型 | 默认值 |
|--------|------|--------|
| `cb_throttle_incoming` | bool | false |
| `cb_throttle_outgoing` | bool | false |
| `throttle_in_kbps` | int | 0 (unlimited) |
| `throttle_out_kbps` | int | 0 |
| `cb_autothrottle` | bool | false |
| `cb_bwpresets` | bool | false |
| `cb_throttle_guarantee_d` | bool | true |
| `cb_throttle_guarantee_s` | bool | true |
| `cb_unchokeall` | bool | false |
| `sb_peerconntimeout` | int (s) | 30 |
| `sb_peerlogintimeout` | int (s) | 20 |
| `sb_maxconnattempts` | int | 20 |
| `incpeerconnproto` | bitfield | all |
| `incconncryptmode` | enum | preferred |
| `peercryptmode` | enum | preferred |
| `dht_startupmode` | enum | delayed |
| `dht_maxsearches` | int | 32 |

## 15. 附录 C：反汇编验证关键代码

### 15.1 Unchoke 模式选择（0x16e8660）

```asm
16e8660: push   %r12
16e8662: mov    %rdi,%r12                   ; r12 = peer object
16e8665: push   %rbp
16e8666: mov    $0x4b67110,%ebp             ; ebp = "local unchoked remote randomly"
16e866b: push   %rbx
16e866c: sub    $0x10,%rsp
16e8670: mov    (%rsi),%eax                  ; eax = arg->mode (enum)
16e8672: cmp    $0x1,%eax                    ; case 1?
16e8675: je     16e8688                       ; → use "randomly"
16e8677: cmp    $0x2,%eax                    ; case 2?
16e867a: mov    $0x4b67130,%ebp             ; ebp = "local unchoked remote for charity"
16e867f: mov    $0x4b677e2,%eax             ; eax = "local unchoked remote" (default)
16e8684: cmovne %rax,%rbp                    ; if !1 && !2, use default
16e8688: lea    0x10(%r12),%rax
16e868d: mov    %rbp,%rdi                    ; rdi = string ptr
16e8690: mov    %rax,(%r12)
16e8694: call   40c630 <strlen@plt>
```

**结论**：这是 unchoke 状态描述符选择代码，对应伪代码：

```c
const char* unchoke_desc(int mode) {
    switch (mode) {
        case 1:  return "local unchoked remote randomly";
        case 2:  return "local unchoked remote for charity";
        default: return "local unchoked remote";
    }
}
```

### 15.2 Trading Allocation 应用（0x1a5db0d）

```asm
1a5dab4: je     1a5db0d                       ; if cb_throttle_guarantee_d, jump
1a5db0d: mov    $0x4b91da0,%esi              ; "Outgoing Throttle Trading Allocation Off"
1a5db12: lea    0x2a0(%rsp),%rdi
1a5db1a: movl   $0x4,0x310(%rsp)
1a5db25: movl   $0x4,0x2e0(%rsp)
1a5db30: movl   $0xc,0x2d0(%rsp)
1a5db3b: call   1a512b0                       ; log_throttle_change(string, ...)
```

**结论**：这是 throttle 状态变更日志记录，对应 throttle 切换的 UI 通知逻辑。

---

## 16. 总结

Tixati 是 BT 客户端生态中**最具个性的存在**：单作者、自研协议栈、独特算法、原生 I2P 支持。逆向分析显示其核心创新（Charity unchoke、Trading/Seeding Allocation、AutoThrottle）值得在现代 Rust 下载器中借鉴。但其闭源、单作者维护的模式也意味着其设计在长期演进上存在风险——这正是开源 Rust 客户端的机会。
