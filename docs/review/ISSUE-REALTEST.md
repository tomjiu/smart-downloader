## 任务

按 `docs/review/REALTEST-PLAN.md` 执行一轮**实机测试**，主线开发专项处理。

**不是**再跑一遍 `cargo test`，而是补上当前完全缺失的四个维度：

| 维度 | 现状 | 目标 |
|---|---|---|
| **A. 前后端契约** | **零**（无 OpenAPI、无 schema、UI 零测试） | 23 条契约矩阵全绿 |
| **B. 并发与死锁** | 单测有，并发压测无 | 7 个场景 + 超时断言 |
| **C. 进程/资源生命周期** | 无 | 删任务后文件停增、关窗后 daemon 退出 |
| **D. 安全** | 后端有校验，无实机验证 | 6 项攻击全拒 |

---

## 为什么必须做这一轮

### 硬证据：限速字段名不一致，现有测试永远发现不了

```rust
// 后端测试 bt_api.rs:454 —— 字段名是对的
.json(&serde_json::json!({ "down_kb_s": 512 }))
assert_eq!(snap["limits"]["down_kb_s"], 512);   // ✅ 通过
```

```typescript
// UI store.ts:94-99 —— 字段名是错的
body: JSON.stringify({ max_download_kb_s: down, max_upload_kb_s: up })
```

**后端测试绿、UI 零测试 → 契约断裂永远发现不了。**

而且 `LimitReq` 的两个字段都是 `#[serde(default)] Option<u32>`，
serde **静默丢弃未知字段** → 不报 400/422 → **UI 显示「限速已应用」，实际空操作**。

### 用户已实测到的问题

| 现象 | 状态 |
|---|---|
| 删任务后后台仍在下载 | ❌ 未修（Issue #120） |
| 关窗后 daemon 仍在运行 | ✅ main 已修，但强杀场景仍有残余风险 |

### 为什么「终端跑普通测试」不够

现有测试的共同特征是**测「模块自己」，不测「模块之间」**：

```
test_should_retry_returns_true_for_429()   ← 测函数返回 ✅
                                          ← 缺「429 时下载真能成功」❌

test_limit_req_deserializes()             ← 测后端能解析 ✅
                                          ← 缺「UI 发的字段后端认识吗」❌

test_remove_with_calls_engine()           ← 测正常路径 ✅
                                          ← 缺「引擎失败时任务会不会残留」❌
```

---

## 交付物（四个文件 + 两个脚本）

```
crates/daemon/tests/contract_matrix.rs        ← 23 条契约（核心）
crates/daemon/tests/concurrency_deadlock.rs   ← 7 个并发场景
crates/daemon/tests/lifecycle_leak.rs         ← 资源泄漏
ui/src/lib/daemon/generated/                  ← ts-rs 生成（或 contract.ts 运行时校验）
scripts/test-desktop-lifecycle.ps1            ← 桌面端进程
scripts/test-security.ps1                     ← 安全实机
```

---

## 核心要求（四条原则）

### 原则 1：契约测试用「UI 的请求体」字面量，不用后端 struct

```rust
// ❌ 错：从后端结构体出发（测不出字段名不一致）
.json(&LimitReq { down_kb_s: Some(100), up_kb_s: None })

// ✅ 对：从 UI 源码抄字面量，并标注来源
.json(&serde_json::json!({ "down_kb_s": 100 }))
// 抄自 ui/src/lib/daemon/store.ts:96 —— 改 UI 必须同步改这里
```

### 原则 2：断言「用户可见效果」，不是 HTTP 状态码

```rust
// ❌ 错
assert_eq!(resp.status(), 200);

// ✅ 对
assert!(实测速度 <= 设定值 * 1.1, "限速未生效");
assert!(!part_file_growing, "删任务后仍在写盘");
```

### 原则 3：所有并发测试必须加超时

死锁会挂死 CI，超时让它变成「失败」而非「卡住」。

```rust
tokio::time::timeout(Duration::from_secs(30), test_body())
    .await
    .expect("超时 —— 疑似死锁");
```

### 原则 4：每个测试必须能「真的复现缺陷」

**验证方法：把代码回滚到修复前，测试必须变红。**

```bash
git stash
cargo test --test contract_matrix limit   # 必须 FAIL
git stash pop
cargo test --test contract_matrix limit   # 必须 PASS
```

**不满足这条的测试等于没写。**

---

## 23 条契约矩阵（必测）

| # | 端点 | 关键断言 |
|---|---|---|
| 1-9 | `POST /tasks` 的 url / torrent_b64 / metalink_b64 / headers / auth / sha256 / dest / proxy / start_at_unix | 各自真的生效（mock server 侧校验） |
| **10** | **`POST /tasks/:id/limit`** | **速度真降到设定值** ← 当前坏的 |
| 11-15 | sequential / connections / proxy / piece-priority / super-seeding | 各自生效 |
| **16** | **`GET /tasks/:id`** | **`name` 可为 null，UI 不崩** |
| **17** | **`GET /tasks/:id/logs`** | **响应含 `events:[{at_ms,op,detail}]`** |
| 18-19 | batch / priority | 批量与队列生效 |
| 20 | trackers (GET/POST/DELETE) | 增删查 |
| 21-22 | providers / config/limit | 返回正确 |
| 23 | `PUT /settings` | 持久化 + 生效 |

完整规格见 `REALTEST-PLAN.md` §A-2。

---

## 并发死锁 7 场景

| # | 场景 | 断言 |
|---|---|---|
| 1 | 并发 add 超卖 | 配额 3，并发 50 → 恰好 3 个进引擎 |
| 2 | 并发 remove + add | 无 panic、无残留 |
| 3 | 并发 pause + remove | 引擎无孤儿 |
| 4 | 账本并发写 | 文件不损坏 |
| 5 | RSS 并发修改 | 磁盘不丢失更新 |
| 6 | 段下载并发 | 数据完整、无重叠 |
| 7 | 限速器并发 | 总速率不超标 |

**B-1 回归（刚修的两个 P0）**：
- `cid_store` 二分边界 → 把 `repro/p0repro/round22.rs` 搬进 `crates/xunlei-convert/tests/`
- ABBA 死锁 → 把 `round23.rs` 搬进 `crates/daemon/tests/`

---

## 生命周期测试

```rust
#[tokio::test]
async fn remove_task_stops_download_within_boundary() {
    // 100MB 慢速下载 → 等 2 秒 → 删任务
    // → 等 5 秒（段边界）→ 断言 .part 不再增长
}
```

**桌面端脚本**（`scripts/test-desktop-lifecycle.ps1`）：
1. 启动 → 确认 daemon 起来
2. 普通关窗 → 断言 daemon 退出
3. **强杀壳进程** → 断言 daemon 自动退出（需 Job Object / PDEATHSIG）

---

## 安全实机 6 项

| # | 攻击 | 期望 |
|---|---|---|
| 1 | `Host: evil.com`（DNS rebinding） | 403 |
| 2 | `Origin: https://evil.com` | 403 |
| 3 | `Origin: null` | 403 |
| 4 | Host 缺失（HTTP/1.0） | 403 |
| 5 | 路径穿越（`../`、`con`、`.`、`con .txt`） | 拒绝 |
| 6 | 无 token / 错 token | 401（常量时间） |

---

## 必须一并修的 6 个问题

| # | 问题 | 位置 |
|---|---|---|
| 1 | 限速字段名不一致 | `store.ts:94-99` |
| 2 | `name` 可为 null 但 UI 当 string | `types.ts:31` + `TasksView.tsx:60,167` |
| 3 | `logs` 响应形状不符 | `store.ts:91` |
| 4 | `remove_with` 静默失败 | `ops.rs:3631`（Issue #120） |
| 5 | `should_retry` 未接线 | `retry.rs:33`（UNWIRED #1） |
| 6 | `Progress`/`Speed` 无人发布 | PR #117 在修 |

---

## 可直接复用的基础设施（不要从零写）

| 资产 | 位置 |
|---|---|
| mock HTTP server（Range/429/ETag/慢速） | `crates/httpdl/tests/integration/http_server.rs` |
| 测试 daemon 启动器 | `crates/daemon/tests/common/` |
| `FakeEngine`（记录调用、可控错误） | `crates/core/tests/mocks/mock_engine.rs` |
| `LT_SESSION_GATE`（BT 串行化） | `crates/daemon/tests/common/lt_gate.rs` |
| 现有复现脚本 | `repro/p0repro/round22-24.rs` |

---

## 验收标准

**必须全部满足：**

1. **契约矩阵 23 条全绿**，每条断言用户可见效果
2. **并发测试无超时**（全部带 `tokio::time::timeout`）
3. **删任务后 `.part` 停止增长**
4. **关窗后 daemon 退出**（普通关闭 + 强杀两种）
5. **安全测试 6 项全拒**
6. **每个测试都能「回滚到修复前变红」**

---

## 估时

**9.5 个工作日**（详见 `REALTEST-PLAN.md` §6）

| 阶段 | 内容 | 估时 |
|---|---|---|
| P0 | 契约基线 + 23 条矩阵 + 修 3 个不一致 | 3.5 天 |
| P1 | 并发死锁 + 生命周期 + 桌面脚本 | 3 天 |
| P2 | Playwright e2e + 安全实机 | 3 天 |

---

## 附：完整方案

`docs/review/REALTEST-PLAN.md`（含每个测试的代码骨架）
