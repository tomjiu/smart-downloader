# 多协议下载器逆向分析与 Rust 原型 - 总览

## 项目目标
分析 5 个主流下载器（qBittorrent, FileCentipede, FlashGet, Tixati, 夸克网盘）的架构和核心算法，
为新开发的 Rust 多协议下载器提供设计依据。

## 目录结构
```
multi_downloader_analysis/
├── README.md                                # 本文档
├── worklog.md                               # 完整工作日志（多 agent 协作记录）
│
├── analysis/                                # 分析文档
│   ├── 01_qbittorrent/                      # qBittorrent 源码分析 (97KB, ~11000 字)
│   │   └── qbittorrent_architecture.md
│   ├── 02_filecentipede/                    # FileCentipede 源码分析 (88KB, ~9000 字)
│   │   └── filecentipede_architecture.md
│   ├── 03_flashget/                         # FlashGet 历史架构分析 (87KB, ~8500 字)
│   │   └── flashget_architecture.md
│   ├── 04_tixati/                           # Tixati 闭源二进制逆向 (53KB, ~8000 字)
│   │   └── tixati_architecture.md
│   ├── 05_quark/                            # 夸克网盘 installer 逆向 (34KB, ~5000 字)
│   │   └── quark_architecture.md
│   ├── 06_comparison/                       # 五大客户端横向对比 (26KB)
│   │   └── cross_client_comparison.md
│   └── 07_rust_proto/                       # Rust 原型代码 (36 文件, ~6000 行)
│       └── multi_downloader/
│           ├── Cargo.toml
│           ├── README.md
│           ├── src/                          # 8 模块 36 文件
│           └── examples/                    # 3 个示例
│
└── reversing_evidence/                      # 逆向证据材料
    ├── strings_dump/                        # strings 提取
    │   ├── tixati_strings.txt               # 133K 行字符串
    │   ├── quark_mini_install_strings.txt   # 11K 行字符串
    │   ├── tixati_dynsym.txt                # 动态符号
    │   └── ...
    └── decompiled/
        └── quark/                           # 夸克 PE 资源解压
            ├── zipres_extracted/            # UI 资源 (PNG + XML)
            └── dll_extracted/               # mini_install.dll (3.7MB)
```

## 5 大客户端分析摘要

| 客户端 | 类型 | 分析方法 | 核心发现 |
|--------|------|----------|----------|
| qBittorrent | 开源 C++ | 源码静态分析 | libtorrent wrapper, alert 系统, settings_pack |
| FileCentipede | 半开源 C++ | 源码 + strings | 6 引擎抽象 + 4 层嗅探 + 双进程 IPC |
| FlashGet | 历史 | 公开资料 + 对照 | 多线程分段 + Mirror 加权评分 + P4S 失败教训 |
| Tixati | 闭源 ELF | 反汇编 + lief | 自研 BT + Charity unchoke + Trading Allocation + 5 层带宽 |
| 夸克网盘 | 闭源 PE | PE 资源解压 + strings | InnoSetup + mini_install.dll + 7 阶段状态机 + 三段错误码 |

## Rust 原型已实现的算法

- Tixati Peer 评分算法（7 字段加权，带 7 个单元测试）
- Tixati 3 模式 Unchoke（Forced/Random/Charity，4 个测试）
- Tixati 5 层带宽分配（Global+Trading+Seeding+AutoLimit+Quota）
- Tixati AutoThrottle RTT 自动限速
- Tixati 11 阶段连接生命周期状态机
- Quark 7 阶段安装状态机
- Quark 三段错误码
- Quark DownloadEventListener trait
- FlashGet Mirror 加权评分公式
- FlashGet 6 状态 Part 状态机
- FlashGet Keep-Alive socket 池
- FileCentipede 三层嗅探规则引擎

## 关键架构决策

1. ✅ 用 librqbit 替代自研 BT 协议栈（避免 Tixati 90MB 教训）
2. ✅ 用 SQLite WAL 替代 .jc! 嵌入（避免 FlashGet 教训）
3. ✅ 用 rustls 替代 OpenSSL（避免 Quark 静态 OpenSSL 体积）
4. ✅ 用 AEAD 替代 RC4（避免 Tixati MSE 过时加密）
5. ✅ 零埋点上报（避免 Quark 4 个上报通道的隐私问题）
6. ✅ 单一可执行（避免 Quark InnoSetup + DLL 双层）
7. ✅ webpki-roots 跨平台（避免 Quark Windows cert store 依赖）
8. ✅ Mirror 默认关闭（避免 FlashGet P4SP 教训）
9. ✅ 开源 MIT/Apache（避免 Tixati/Quark 闭源问题）

## 如何继续

1. 安装 rustc + cargo
2. cd analysis/07_rust_proto/multi_downloader
3. cargo test        # 运行单元测试
4. cargo build --release
5. ./target/release/mdc download "https://example.com/file.zip" --out ./file.zip --concurrency 4

未来工作：
- 接入 librqbit 实现 BT 协议栈本体（替换 bt_engine.rs 占位）
- 实现 MSE/PE 加密（基于 AEAD）
- 实现 uTP 协议
- 实现 DHT (BEP 5) + PEX (BEP 11) + LSD (BEP 14)
- 实现浏览器扩展（FileCentipede 风格）
