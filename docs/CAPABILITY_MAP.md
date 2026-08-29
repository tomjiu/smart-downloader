# 下载器能力地图（CAPABILITY_MAP）

> 创建：2026-08-25。来源：ChatGPT 对话修正版 + 本项目现状对齐。
> 定位：**远期专项的立项依据库**——不按软件排，按「现有内核缺失且值得抽取的能力」排。
> 前置状态：主线封版待验收（F3.1 + G1/G2）；迅雷线登录已解决；本文件全部条目**门控**在
> 「F3.1 完成 + Bug B/C 收口」之后启动。

---

## 一、终局架构图（能力地图的目标形态）

```
                    smart-dl Rust 调度层
                           │
          ┌────────────────┼────────────────┐
          │                │                │
      Source 发现       Piece 调度        传输能力
          │                │                │
    ┌─────┼─────┐     ┌────┼────┐      ┌────┼────┐
    │     │     │     │    │    │      │    │    │
  DHT  Tracker PEX  Rarest Seq Stream TCP uTP HTTP
    │
    ├── BitComet LT-Seeding        （云解析中）
    ├── 迅雷 PHub/资源发现          ← S2-G6 信封自建已开门
    ├── BiglyBT Swarm Merging      ← 开源直读首选
    ├── eMule Kad/Source Exchange  ← 抽象为 SourceProvider
    └── WebTorrent WebRTC          ← 远期
                 ↓
            Source Pool（统一来源池）
                 ↓
            Piece Manager（跨源去重合并）
                 ↓
              File Writer
```

**已实现对照**：Source Pool 的雏形=Provider fallback + webseeds 注入端点；
Piece 合并=libtorrent 原生；多连接 Range=httpdl。DHT/PEX/uTP=内核内置开关
（M0 关闭决策，G1 手动验证脚本已备）。

---

## 二、净增量四项（剔除已实现/内置后的真正增量）

| # | 能力 | 来源样本 | 对接点 |
|---|------|---------|--------|
| N1 | **Swarm Merging**：多 torrent 同内容 piece 可用性合并 | BiglyBT | 调度层新增「同 gcid/v2 hash 群组」→ 多任务共享 piece 池 |
| N2 | **Source Exchange 抽象**：Kad 的"问谁有这文件"泛化 | eMule/MLDonkey | `trait SourceProvider { async fn discover_sources(...) }` |
| N3 | **Streaming / Sequential**：边下边播的顺序下载模式 | μT/qB/玩家需求 | httpdl 顺序段模式 + BT piece 优先级 |
| N4 | **Super Seeding + 磁盘缓存策略** | μT/Tixati/BitComet | btcore 内核开关与缓存参数（需内核暴露时再议） |

---

## 三、客户端分档

### A. 开源直读（本项目代理可直接执行，读代码抽思想）
| 对象 | 语言 | 抽什么 | 门控 |
|------|------|--------|------|
| **BiglyBT** | Java | Swarm Merging 实现（内容识别/piece 映射/合并调度） | F3.1 后 |
| **Transmission** | C | 独立引擎对照：piece picker/请求窗口/choking | N4 时 |
| **aria2** | C++ | 多协议 Source/Segment Scheduler（httpdl 对照升级） | 主线后 |
| **Deluge** | Python | libtorrent 上层策略对照（隔离引擎贡献） | 与 Transmission 并行 |
| **eMule/MLDonkey** | C++/C | Kad、Source Exchange、AICH 哈希 | N2 时 |
| **WebTorrent** | JS | WebRTC transport adapter 形态 | 远期 |
| **Tribler** | Python | 匿名网络/内容发现结合下载 | 远期 |

### B. ☁️ 闭源 → 云 AI 解析队列（本文档核心新增）
> 流程：云解析交付 → 本项目审计（按 §四 审查单）→ 转化为立项或归档。
> 状态标记：☁️ 待派 / 📥 已派 / ✅ 已审。

| 状态 | 对象 | 云解析必答考题 |
|------|------|---------------|
| 📥 已派 | **BitComet** | ①LT-Seeding 完整协议（LT-hash 算法/发现服务器/握手/认证）②Torrent Exchange 载体与交换粒度 ③HTTP/FTP P2P 如何借力 ④Anti-Leech 多级策略 ⑤磁盘缓存分层 |
| ☁️ 待派 | **μTorrent / BT Classic** | 同 swarm 内更快建连的策略差异：uTP 参数/连接配比/choke 算法/piece 选择/上传槽位/磁盘缓存写回策略 |
| ☁️ 待派 | **Tixati** | Peer 质量评分算法、带宽分配模型、连接生命周期管理 |
| ☁️ 待派 | **FlashGet** | 历史多线程/镜像发现逻辑（对照 BitComet HTTP/FTP P2P） |
| ☁️ 待派 | **文件蜈蚣** | 协议嗅探覆盖面（C 档，最后） |
| ✅ 本地已覆盖 | **迅雷本体** | 登录/云盘/加速/配额已完成（见 xunlei 系列 md）；xunlei-ffi 引擎身份 setter 已接线（identity.rs 三 setter + builder）；残余=VipSpeedUpUrl 精确路径（需 Frida）+ cert 下发流程未知 + PHub 自建（S2-G6） |

### C. 明确不分析
qBittorrent（基线本体）、Motrix（=aria2 皮）、Gopeed（Go 参考非研究对象）。

---

## 四、云解析交付审查单（每份结果按此验收，通过才转立项）

1. **行为证据**：抓包/配置/日志/内存任一实证，不接受纯推测
2. **机制假说**：触发条件、状态机、关键参数可复述
3. **可复刻映射**：逐条映射到 SourceProvider / PieceManager / 传输层 /
   或标注「依赖官方服务端，不可复刻」及原因
4. **成本评估**：预估实现工作量（天级/周级）与依赖（是否需要额外服务端配合）
5. **立项建议**：进 BACKLOG 哪一档、前置条件是什么

---

## 五、启动门控与顺序

```
门控：F3.1 验收通过 + Bug B/C 关闭
  ↓
第一波：BiglyBT Swarm Merging 源码精读（N1）＋ BitComet 云解析结果审计
  ↓
第二波：aria2 scheduler 对照（httpdl 升级输入）＋ Transmission/Deluge 对照实验设计
  ↓
第三波：N2 Source Exchange 抽象立项评估 ＋ 黑盒观察（视需求）
```

## 六、与本项目的对接备忘

- SourceProvider trait 雏形已在 BACKLOG「未来愿景」预留
- webseeds 端点 = Source Pool 的第一个外部源适配器（今日已上线）
- 迅雷 Provider = 第二个（登录已解决，F3.1 待验收）
- 解压 API（F3.2 挂起）若打通 = 「云端预处理」类能力的第一个样例
