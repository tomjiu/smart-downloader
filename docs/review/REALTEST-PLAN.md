# 实机测试方案 —— 前后端契约 + 安全 + 死锁

> 触发原因：用户报告「前端删任务后后台仍在下载」等**前后端行为不一致**问题。
> 要求：**不是终端跑普通测试**，而是专门一轮实机验证。
> 日期：2026-10-19

---

## 0. 为什么现有测试发现不了这些问题

### 硬证据

| 层 | 测试规模 | 覆盖问题能力 |
|---|---|---|
| 后端单元 + 集成 | **26 个 API 测试 + 28 个 httpdl e2e**，含 mock HTTP server | 很强 |
| **UI** | **0 个测试文件** | **零** |
| **前后端契约** | **无 OpenAPI / 无 schema / 无契约测试** | **零** |

### 典型案例：限速字段名不一致

```rust
// 后端测试 bt_api.rs:454 —— 用的是对的字段名
.json(&serde_json::json!({ "down_kb_s": 512 }))
assert_eq!(snap["limits"]["down_kb_s"], 512);   // ✅ 测试通过
```

```typescript
// UI store.ts:94-99 —— 用的是错的字段名
body: JSON.stringify({ max_download_kb_s: down, max_upload_kb_s: up })
```

**后端测试绿，UI 无测试 → 契约断裂永远发现不了。**

而且 `LimitReq` 两个字段都是 `#[serde(default)] Option<u32>`，
serde **静默丢弃未知字段** → 不报 400/422 → **UI 以为成功，实际空操作**。

### 为什么「终端跑测试」不够

现有测试的问题不是数量不够，而是**测的边界画错了**：

```
test_should_retry_returns_true_for_429()     ← 测函数返回值 ✅
                                            ← 缺「429 时下载真的能成功」❌

test_limit_req_deserializes()                ← 测后端能解析 ✅
                                            ← 缺「UI 发的字段后端认识吗」❌

test_remove_with_calls_engine()              ← 测正常路径调用了引擎 ✅
                                            ← 缺「引擎失败时任务会不会残留」❌
```

**共同特征：测「模块自己」，不测「模块之间」。**

---

## 1. 本轮实机测试的四个维度

### 维度 A：前后端契约（**最高优先级**）

**目标**：UI 的每一次 API 调用，都被真实 daemon 验证过。

### 维度 B：并发与死锁

**目标**：高并发下的锁序、超卖、丢失更新被实际压测过。

### 维度 C：进程与资源生命周期

**目标**：删除/暂停/退出后，进程、文件、句柄确实释放。

### 维度 D：安全

**目标**：鉴权、路径穿越、SSRF、CSRF 在真实网络栈上验证。

---

## 2. 维度 A：前后端契约测试

### A-1. 建立契约基线（**第一件事**）

**问题根因**：后端 struct 和前端 TypeScript 类型是两套手写定义，没有单一真源。

**方案（按成本排序）**：

#### 方案 1（推荐，成本最低）：从 Rust 生成 TypeScript 类型

```toml
# crates/daemon/Cargo.toml
[dev-dependencies]
ts-rs = "10"
```

```rust
#[derive(TS)]
#[ts(export, export_to = "../../ui/src/lib/daemon/generated/")]
pub struct LimitReq {
    #[serde(default)] pub down_kb_s: Option<u32>,
    #[serde(default)] pub up_kb_s: Option<u32>,
}
```

```json
// ui/package.json
"scripts": {
  "gen:types": "cargo test -p smart-dl-daemon export_bindings"
}
```

**收益**：字段名不一致在**编译期**就暴露（TS 类型对不上 → `bun run build` 失败）。
**成本**：约 1 天（给 30+ 个 struct 加 derive）。

#### 方案 2：运行时契约断言（零侵入）

在 UI 的 `store.ts` 里，每个请求都带上**开发模式下**的响应校验：

```typescript
// ui/src/lib/daemon/contract.ts —— 仅 dev 模式启用
const CONTRACTS = {
  "/tasks/:id/limit": {
    request: { down_kb_s: "number?", up_kb_s: "number?" },
    response: { limits: "object" },
  },
  // ...
} as const;

function assertContract(path: string, req: unknown, resp: unknown) {
  if (!import.meta.env.DEV) return;
  const spec = matchPath(path);
  if (!spec) {
    console.error(`[contract] 未登记的端点: ${path} —— 请补契约`);
    return;
  }
  // 校验请求体字段名是否在 spec 里
  for (const k of Object.keys(req ?? {})) {
    if (!(k in spec.request)) {
      console.error(`[contract] ${path} 发送了后端不认识的字段: ${k}`);
    }
  }
  // 校验响应必需字段
  for (const k of Object.keys(spec.response)) {
    if (!(k in (resp as object))) {
      console.error(`[contract] ${path} 响应缺少字段: ${k}`);
    }
  }
}
```

**收益**：零构建期改动，运行 UI 就自动发现。
**成本**：约 0.5 天。

#### 方案 3（兜底）：e2e 冒烟测试

用 Playwright 驱动真实 UI + 真实 daemon：

```typescript
// ui/e2e/contract.spec.ts
test("限速真的生效", async ({ page }) => {
  await page.goto("http://127.0.0.1:8788/");
  await addTask(page, "http://localhost:9000/testfile.bin?size=100MB");
  await page.click("[data-testid=task-row]");
  await page.fill("[data-testid=limit-down]", "100");   // 100 KB/s
  await page.click("[data-testid=apply-limit]");

  // 关键断言：等 5 秒后速度必须 ≤ 100 KB/s
  await page.waitForTimeout(5000);
  const speed = await page.textContent("[data-testid=task-speed]");
  expect(parseKBps(speed)).toBeLessThanOrEqual(110);   // 留 10% 余量
});
```

**收益**：验证的是**用户可见效果**，不是函数返回值。
**成本**：约 2 天（含 Playwright 环境）。

**建议：方案 1 + 方案 3 组合**
- 方案 1 保证字段名一致（编译期）
- 方案 3 保证端到端行为一致（运行时）

### A-2. 必测的契约清单（逐条）

**这是本轮实机测试的核心产出物。**

| # | 端点 | 请求字段 | 断言 |
|---|---|---|---|
| 1 | `POST /tasks` | `url` | 任务出现 |
| 2 | `POST /tasks` | `torrent_b64` | .torrent 能加 |
| 3 | `POST /tasks` | `metalink_b64` | metalink 能加 |
| 4 | `POST /tasks` | `headers` | 请求头真的下发（mock server 校验） |
| 5 | `POST /tasks` | `username`/`password` | Basic auth 真的带上 |
| 6 | `POST /tasks` | `sha256` | 校验失败时任务标 Error |
| 7 | `POST /tasks` | `dest` | 落盘到指定目录 |
| 8 | `POST /tasks` | `proxy` | 真的走代理 |
| 9 | `POST /tasks` | `start_at_unix` | 到点才启动 |
| 10 | **`POST /tasks/:id/limit`** | **`down_kb_s`/`up_kb_s`** | **速度真的降到设定值** ← 当前坏的 |
| 11 | `POST /tasks/:id/sequential` | `sequential` | 顺序下载生效 |
| 12 | `POST /tasks/:id/connections` | `max_connections` | 连接数受限 |
| 13 | `POST /tasks/:id/proxy` | `proxy` | 动态换代理 |
| 14 | `POST /tasks/:id/piece-priority` | `priority` | 首尾块优先 |
| 15 | `POST /tasks/:id/super-seeding` | `enabled` | 超级种子开启 |
| 16 | `GET /tasks/:id` | — | **响应形状**：`name` 可为 null |
| 17 | **`GET /tasks/:id/logs`** | — | **响应形状**：`events:[{at_ms,op,detail}]` |
| 18 | `POST /tasks/batch` | `ids`/`op` | 批量操作生效 |
| 19 | `POST /tasks/:id/priority` | 优先级字段 | 队列顺序变化 |
| 20 | `GET/POST/DELETE /tasks/:id/trackers` | `url` | tracker 增删查 |
| 21 | `GET /providers` | — | 返回已装配的 provider 列表 |
| 22 | `POST /config/limit` | `max_download_kb_s` | 全局限速生效 |
| 23 | `PUT /settings` | 各域 | 设置真的持久化 + 生效 |

**每一条都要：**
1. 用**真实 UI** 发起（或完全复刻 UI 的请求体）
2. 断言**用户可见效果**，不是 HTTP 200
3. 记录到 `docs/review/CONTRACT-MATRIX.md`

### A-3. 契约测试的最小实现

```rust
// crates/daemon/tests/contract_matrix.rs
//! 契约矩阵：每条用例都复刻 UI 的真实请求，断言用户可见效果。
//!
//! 与既有测试的区别：既有测试用「后端视角」的请求体，
//! 这里用「UI 视角」的请求体 —— 字段名不一致会立刻失败。

#[tokio::test]
async fn contract_limit_actually_limits_speed() {
    // 1. 起 mock 下载服务器（用现成的 tests/integration/http_server.rs）
    let srv = TestServer::builder()
        .route("/file.bin", SlowBody::new(10 * 1024 * 1024, 200))  // 10MB @ 200ms/256KB
        .start();

    // 2. 起 daemon
    let daemon = TestDaemon::start().await;

    // 3. 加任务
    let tid = daemon.add_task(&srv.url("/file.bin")).await;

    // 4. **完全复刻 UI 的请求体**
    let resp = daemon
        .client()
        .post(format!("{}/tasks/{}/limit", daemon.base(), tid))
        .json(&serde_json::json!({
            // 从 ui/src/lib/daemon/store.ts 抄过来 —— 改 UI 必须同步改这里
            "down_kb_s": 100, "up_kb_s": null
        }))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // 5. 关键断言：5 秒内下载量必须 ≤ 100KB/s × 5s + 余量
    let before = daemon.status(&tid).await.downloaded;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let after = daemon.status(&tid).await.downloaded;
    let kbps = (after - before) as f64 / 1024.0 / 5.0;

    assert!(kbps <= 120.0,
        "限速 100KB/s 但实测 {kbps:.1}KB/s —— 契约失效");
}

#[tokio::test]
async fn contract_logs_response_shape() {
    let daemon = TestDaemon::start().await;
    let tid = daemon.add_task("http://example/x").await;

    let body: serde_json::Value = daemon.get(&format!("/tasks/{tid}/logs")).await;

    // 断言 UI 期望的字段存在
    assert!(body["events"].is_array(), "缺少 events 字段: {body}");
    if let Some(ev) = body["events"].as_array().and_then(|a| a.first()) {
        assert!(ev["at_ms"].is_number(), "event 缺 at_ms");
        assert!(ev["op"].is_string(), "event 缺 op");
    }
}

#[tokio::test]
async fn contract_task_name_may_be_null() {
    let daemon = TestDaemon::start().await;
    // 用无法推导名字的 URL（纯 query，无路径段）
    let tid = daemon.add_task("http://example.com/?id=123").await;
    let body: serde_json::Value = daemon.get(&format!("/tasks/{tid}")).await;

    // name 可以是 null，但 UI 必须能处理
    assert!(body.get("name").is_some() || body["name"].is_null(),
        "name 字段应存在或为 null");
}
```

---

## 3. 维度 B：并发与死锁实机测试

### B-1. 已有缺陷的回归（两个刚修的 P0）

**P0-1 `cid_store` 二分边界**：已在 `repro/round22.rs` 验证
（249,964 检查点，修复后 0 差异）。
**要求**：把 round22 的逻辑搬进 `crates/xunlei-convert/tests/`，变成常驻回归。

**P0-2 ABBA 死锁**：已在 `repro/round23.rs` 验证
（8 线程 × 2000 次，修复后 16000/16000 全过）。
**要求**：搬进 `crates/daemon/tests/`，并加超时断言。

```rust
// crates/daemon/tests/concurrency_deadlock.rs
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_add_and_attach_do_not_deadlock() {
    let state = Arc::new(DaemonState::new(FakeEngine::new(EngineKind::Http), vec![]));

    let mut handles = Vec::new();
    for t in 0..8 {
        let s = state.clone();
        handles.push(tokio::spawn(async move {
            for k in 0..100 {
                let task = make_task(&format!("t{t}-{k}"));
                // gate_or_enqueue + attach_reserved_handle 并发
                if s.gate_or_enqueue(&task, EngineKind::Http) {
                    s.attach_reserved_handle(
                        &task.id, format!("et-{t}-{k}"),
                        EngineKind::Http, &TaskState::Queued
                    ).await;
                }
            }
        }));
    }

    // 关键：超时断言。死锁时会挂住
    tokio::time::timeout(Duration::from_secs(30), futures::future::join_all(handles))
        .await
        .expect("并发 add + attach 超时 —— 疑似死锁");
}
```

### B-2. 其它并发场景

| # | 场景 | 断言 |
|---|---|---|
| 1 | 并发 add 超卖 | 并发 50 个 add，配额 3 → 恰好 3 个进引擎 |
| 2 | 并发 remove + add | 无 panic、无残留记录 |
| 3 | 并发 pause + remove | 最终态一致，引擎无孤儿 |
| 4 | 账本并发写 | 两个 worker 同时 save → 文件不损坏 |
| 5 | RSS 并发修改 | 并发 add/update feed → 磁盘状态不丢失 |
| 6 | 段下载并发 | 8 连接下分段不重叠、数据完整 |
| 7 | 限速器并发 | 多任务共享全局限速 → 总速率不超标 |

### B-3. 死锁检测工具

```bash
# 用 loom 做模型检查（重）
cargo install loom

# 或用 tokio-console 观测运行时任务（推荐）
cargo add tokio-console --dev
RUSTFLAGS="--cfg tokio_unstable" cargo test --features tokio-console
```

**更实用的做法：所有并发测试都加 `tokio::time::timeout`**
—— 死锁时测试会超时失败，而不是挂死 CI。

---

## 4. 维度 C：进程与资源生命周期

### C-1. 删任务后引擎真的停了吗（**用户报告的问题**）

```rust
// crates/daemon/tests/lifecycle_leak.rs
#[tokio::test]
async fn remove_task_stops_download_within_boundary() {
    let srv = TestServer::slow_body(100 * 1024 * 1024, 1);  // 100MB 慢速
    let daemon = TestDaemon::start().await;
    let tid = daemon.add_task(&srv.url("/big.bin")).await;

    // 等下载起来
    tokio::time::sleep(Duration::from_secs(2)).await;
    let part = daemon.part_path(&tid);
    let size_before = fs::metadata(&part).unwrap().len();

    // 删任务
    daemon.delete(&tid).await.expect("删除应成功");

    // 等待段边界退出（最长 1 个段 = 16MB / 速度）
    tokio::time::sleep(Duration::from_secs(5)).await;
    let size_after = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

    assert_eq!(size_after, 0,
        "删任务后 .part 仍在增长（{size_before} → {size_after}）—— 引擎未停止");
}

#[tokio::test]
async fn remove_task_propagates_engine_error() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    fake.set_remove_error(EngineError::Other("simulated".into()));
    let state = DaemonState::new(fake, vec![]);
    let tid = add_one(&state).await;

    let r = state.remove_with(&tid, false).await;

    assert!(r.is_err(), "引擎失败必须传播给调用方");
    assert!(state.tasks.lock().get(&tid).is_some(),
        "失败时任务记录必须保留（可重试）");
}
```

### C-2. 关窗后 daemon 真的退出了吗

**这是桌面端问题，需要真机验证：**

```powershell
# scripts/test-desktop-lifecycle.ps1

# 1. 启动桌面应用
$app = Start-Process "smart-downloader.exe" -PassThru
Start-Sleep -Seconds 5

# 2. 确认 daemon 起来了
$daemon = Get-Process smart-dl-daemon -EA SilentlyContinue
if (-not $daemon) { throw "daemon 未启动" }
Write-Host "✅ daemon 已启动 PID=$($daemon.Id)"

# 3. 添加一个慢速任务（让它在下载中）

# 4. 关闭窗口（不是任务管理器强杀）
$app.CloseMainWindow()
Start-Sleep -Seconds 5

# 5. 断言 daemon 已退出
if (Get-Process smart-dl-daemon -EA SilentlyContinue) {
    throw "❌ 关窗后 daemon 仍在运行"
}
Write-Host "✅ 关窗后 daemon 已退出"

# 6. 强杀场景（验证 Job Object 加固）
$app2 = Start-Process "smart-downloader.exe" -PassThru
Start-Sleep -Seconds 5
Stop-Process -Id $app2.Id -Force        # 模拟任务管理器强杀
Start-Sleep -Seconds 5
if (Get-Process smart-dl-daemon -EA SilentlyContinue) {
    throw "❌ 强杀壳进程后 daemon 成为孤儿"
}
Write-Host "✅ 强杀后 daemon 已自动退出（需要 Job Object / PDEATHSIG）"
```

### C-3. 文件与句柄泄漏

| # | 检查 | 方法 |
|---|---|---|
| 1 | 删任务后 `.part`/`.progress`/`.etag` 全部清理 | 跑 100 次 add+remove，检查目录为空 |
| 2 | 换源后旧代次 `.part` 清理 | 触发 5 次 update_sources，无 `*.1.part` 残留 |
| 3 | 暂停后账本持久化 | pause → 重启 daemon → 进度保留 |
| 4 | 磁盘句柄释放 | Windows: `handle.exe` 查 daemon 进程的打开文件 |
| 5 | 端口释放 | 退出后 8788 可立即复用 |

---

## 5. 维度 D：安全实机测试

### D-1. 鉴权与 CSRF

| # | 场景 | 期望 |
|---|---|---|
| 1 | 无 token 访问 `/tasks` | 401 |
| 2 | 错误 token | 401（且**常量时间**比较） |
| 3 | DNS rebinding（`Host: evil.com`） | 403 |
| 4 | 跨源 POST（`Origin: https://evil.com`） | 403 |
| 5 | `Host` 缺失（HTTP/1.0） | 403 |
| 6 | `Origin: null`（沙箱 iframe） | 403 |

**实测方法**：用 `curl` 手工构造，或用 reqwest 裸发。

```bash
# 1. DNS rebinding
curl -H "Host: evil.com" http://127.0.0.1:8788/tasks

# 2. 跨源
curl -X POST -H "Origin: https://evil.com" http://127.0.0.1:8788/tasks

# 3. 无 Host（curl 默认会带，用 --http1.0）
curl --http1.0 -H "Host:" http://127.0.0.1:8788/tasks
```

### D-2. 路径穿越

```rust
#[tokio::test]
async fn path_traversal_blocked() {
    let daemon = TestDaemon::start().await;
    for evil in [
        "../../../etc/passwd",
        "..\\..\\..\\windows\\system32\\config\\sam",
        "con",                    // Win32 设备名
        "con .txt",               // 尾随空格绕过
        "COM1.txt",
        ".",                      // ← CurDir，已知缺陷
        "a/./../../b",
    ] {
        let r = daemon.add_task_with_dest("http://x/f", evil).await;
        assert!(r.is_err(), "路径 {evil:?} 应被拒绝");
    }
}
```

> 注：`sanitize_rel(".")` 的缺陷我已在
> `BACKEND-REVIEW-ROUND2.md` P1-2 报告，batch10 已修。

### D-3. SSRF

| # | 场景 | 期望 |
|---|---|---|
| 1 | FTP PASV 返回内网地址 | 拒绝 |
| 2 | 备用源指向 `127.0.0.1` | 按策略处理 |
| 3 | 重定向到内网 | 可配限制 |

### D-4. 凭证安全

| # | 检查 |
|---|---|
| 1 | `tasks.json` / `auth.json` 权限 0600 |
| 2 | 日志不输出 token/cookie |
| 3 | `/settings` 快照不泄露凭据（只暴露开关） |

---

## 6. 执行计划

### 阶段划分

| 阶段 | 内容 | 估时 | 产出 |
|---|---|---|---|
| **P0** | 契约基线：ts-rs 生成类型 | 1 天 | `ui/src/lib/daemon/generated/` |
| **P0** | 契约矩阵测试（23 条） | 2 天 | `crates/daemon/tests/contract_matrix.rs` |
| **P0** | 修限速 & logs & name 三个不一致 | 0.5 天 | — |
| **P1** | 并发死锁测试（7 场景） | 1.5 天 | `crates/daemon/tests/concurrency_deadlock.rs` |
| **P1** | 生命周期泄漏测试 | 1 天 | `crates/daemon/tests/lifecycle_leak.rs` |
| **P1** | 桌面端生命周期脚本 | 0.5 天 | `scripts/test-desktop-lifecycle.ps1` |
| **P2** | Playwright e2e（5 条关键路径） | 2 天 | `ui/e2e/` |
| **P2** | 安全实机测试 | 1 天 | `scripts/test-security.ps1` |
| | **合计** | **9.5 天** | |

### 验收标准

**必须全部满足才算通过：**

1. **契约矩阵 23 条全绿** —— 每条都断言用户可见效果，不是 HTTP 200
2. **并发测试无超时** —— 所有测试加 `tokio::time::timeout`
3. **删任务后 `.part` 停止增长**
4. **关窗后 daemon 退出**（普通关闭 + 强杀两种）
5. **安全测试 6 项全拒**
6. **UI 端到端 5 条关键路径通过**

### CI 集成

```yaml
# .github/workflows/ci.yml 新增 job
contract-matrix:
  name: 前后端契约矩阵
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - name: 生成 TS 类型并检查一致性
      run: |
        cargo test -p smart-dl-daemon export_bindings
        cd ui && bun run build   # 类型不匹配则失败
    - name: 契约矩阵测试
      run: cargo test -p smart-dl-daemon --test contract_matrix

concurrency:
  name: 并发与死锁
  runs-on: ubuntu-latest
  steps:
    - run: cargo test -p smart-dl-daemon --test concurrency_deadlock
```

---

## 7. 关键原则（写给执行者）

### 原则 1：契约测试用「UI 的请求体」，不是「后端的 struct」

```rust
// ❌ 错：从后端结构体出发（测不出字段名不一致）
.json(&LimitReq { down_kb_s: Some(100), up_kb_s: None })

// ✅ 对：从 UI 源码抄字面量（字段名一改就失败）
.json(&serde_json::json!({ "down_kb_s": 100 }))
// 并在注释里标注来源：// 抄自 ui/src/lib/daemon/store.ts:96
```

### 原则 2：断言「用户可见效果」，不是「HTTP 状态码」

```rust
// ❌ 错
assert_eq!(resp.status(), 200);

// ✅ 对
assert!(实测速度 <= 设定值 * 1.1, "限速未生效");
assert!(!part_file_growing, "删任务后仍在写");
```

### 原则 3：所有并发测试必须超时

```rust
// 死锁会挂死 CI，超时让它变成「失败」而非「卡住」
tokio::time::timeout(Duration::from_secs(30), test_body())
    .await
    .expect("超时 —— 疑似死锁");
```

### 原则 4：测试要能「真的复现缺陷」

**验证测试有效性的方法：把代码回滚到修复前，测试必须变红。**

```bash
# 例：验证 contract_limit_actually_limits_speed 有效
git stash                                  # 回滚到修复前
cargo test --test contract_matrix limit    # 必须 FAIL
git stash pop                              # 恢复
cargo test --test contract_matrix limit    # 必须 PASS
```

**不满足这条的测试，等于没写。**

---

## 8. 附带必须修的问题

这些是审计已发现、必须在本轮一并修的：

| # | 问题 | 位置 | 优先级 |
|---|---|---|---|
| 1 | 限速字段名不一致 | `store.ts:94-99` | P0 |
| 2 | `name` 可为 null 但 UI 当 string | `types.ts:31` + `TasksView.tsx:60,167` | P0 |
| 3 | `logs` 响应形状不符 | `store.ts:91` | P1 |
| 4 | `remove_with` 静默失败 | `ops.rs:3631` | P0（Issue #120） |
| 5 | `should_retry` 未接线 | `retry.rs:33` | P0（UNWIRED #1） |
| 6 | `Progress`/`Speed` 无人发布 | 全仓 | P1（PR #117 在修） |

---

## 9. 参考：现有可直接复用的基础设施

**不要从零写，这些已经存在：**

| 资产 | 位置 | 用途 |
|---|---|---|
| mock HTTP server | `crates/httpdl/tests/integration/http_server.rs` | 支持 Range/429/ETag/慢速 |
| 测试 daemon 启动器 | `crates/daemon/tests/common/` | 起真实 axum + 临时目录 |
| `FakeEngine` | `crates/core/tests/mocks/mock_engine.rs` | 记录调用，可控错误 |
| `LT_SESSION_GATE` | `crates/daemon/tests/common/lt_gate.rs` | BT 测试串行化 |
| `TestDaemon::with_config` | `crates/daemon/tests/http_api.rs` | 注入配置 |
| 复现脚本 | `repro/p0repro/round22-24.rs` | 已含 cid_store/ABBA/remove 验证 |

**契约矩阵测试沿用 `http_api.rs` 的启动模式即可，不需要新框架。**
