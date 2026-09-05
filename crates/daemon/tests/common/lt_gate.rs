//! libtorrent session 进程内串行门（bt 构建测试 flaky 治理）。
//!
//! 根因：libtorrent 2.0 的 `session_params` 默认监听 `0.0.0.0:6881`，且本仓库
//! ffi 层未导出 `listen_interfaces` 设置项，测试无法改端口。cargo test 在同一
//! test binary 内以多线程并行运行用例，多个 session 同时抢 6881 → `Address
//! already in use` / listen 重试抖动 → bt 构建下测试 flaky 的主要来源。
//!
//! 串行化模型：cargo test 默认逐 target 串行执行（每个 test binary 一个进程），
//! 跨 binary 天然无竞争；进程内一把门即可覆盖同文件全部会话创建。若未来引入
//! nextest（每用例一进程），需另加跨进程文件锁（见技术债清单残余项）。
//!
//! 用法（测试体首行，插入由 `scripts/insert_lt_gate.py` 幂等维护）：
//! - 异步用例：`let _lt = crate::common::lt_gate::LT_SESSION_GATE.lock().await;`
//! - 同步用例：`let _lt = crate::common::lt_gate::LT_SESSION_GATE.blocking_lock();`
//!
//! 锁序约定：先取本门，再取 seeder 文件锁（fastresume 类用例），不得反序；
//! 本门不可重入（tokio Mutex 无重入语义，重入即死锁）。

#![allow(dead_code)] // 非 bt 测试 binary 同样 include 本模块，未用即静默

use tokio::sync::Mutex;

/// 进程内 LT session 串行门：同一时刻至多一个 libtorrent session 处于存活期。
pub static LT_SESSION_GATE: std::sync::LazyLock<Mutex<()>> =
    std::sync::LazyLock::new(|| Mutex::new(()));
