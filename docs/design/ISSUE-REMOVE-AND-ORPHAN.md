## 背景

用户报告（0.2.1 旧版本）：

> 前端删掉任务停止了下载，但后台仍然继续；甚至关掉程序窗口它仍在运行。

经核查，**两个问题性质不同**：

| 问题 | 0.2.1 状态 | main 当前状态 |
|---|---|---|
| **关窗后 daemon 仍在** | ❌ 有缺陷 | ✅ **已修**（`ExitRequested` 兜底） |
| **删任务后仍在下载** | ❌ 有缺陷 | ❌ **仍未修** — 本 issue 主体 |

---

# 问题 1：`remove_with` 的静默失败导致「删了还在下」（P0）

## 位置

`crates/daemon/src/state/ops.rs:3631-3644`

## 现状代码

```rust
pub async fn remove_with(&self, id: &str, delete_data: bool) -> Result<(), DaemonError> {
    let rec = self
        .tasks
        .lock()
        .remove(id)                                   // ① 先删记录
        .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;

    if let Some(tid) = rec.engine_tid {
        if let Ok(engine) = self.engine_for(rec.engine_kind) {   // ② 引擎不可用 → 静默跳过
            let _ = engine.remove(&tid, delete_data).await;      // ③ 失败 → 静默吞掉
        }
    }
    self.autosave();
    Ok(())                                            // ④ 恒返回 Ok
}
```

## 三条缺陷路径

### 缺陷 1：先删记录，后停引擎（顺序错误）

① 执行后任务从 `tasks` 表消失 → **UI 立即看不到它**。
随后 ②③ 若失败，**再也没有任何数据结构能追踪这个引擎任务** —— 无法重试、无法清理、无法诊断。

### 缺陷 2：`if let Ok(engine)` —— 引擎不可用时静默跳过

`engine_for()` 返回 `Err` 的场景：
- feature 未启用（如构建未带 `--features bt`）
- 引擎装配失败

此时**引擎侧任务仍在运行，但无人通知它停**，而函数仍返回 `Ok(())`。

### 缺陷 3：`let _ = engine.remove(...)` —— 吞掉所有错误

`engine.remove()` 返回 `NotFound` / `Other` 全部被丢弃：
- 无日志
- 无事件
- 用户无感知

### 缺陷 4：恒返回 `Ok(())`

即使引擎停止失败，调用方（HTTP handler）也收到 204 No Content，
UI 显示「删除成功」。

## 加剧因素：段边界退出

`HttpEngine::remove`（`crates/httpdl/src/engine.rs`）的实现：

```rust
let t = tasks.remove(id).ok_or(EngineError::NotFound)?;
// ...
pause_flag.store(true, Ordering::SeqCst);   // ← 只置标志
```

**worker 在下一个段边界才退出**，段粒度 `DEFAULT_MIN_SPLIT = 16MB`。

所以**即使 remove 成功**，正在写的那个 16MB 段仍会下完才开始下载（这是有意的优雅停止设计，不是缺陷）。
但叠加上述静默失败后，用户看到的就是「**删了，文件还在变大**」。

## 佐证：设计本来是期望传播错误的

`ops.rs:3798` 的自动清扫路径**已经按 `Result` 正确分支**：

```rust
match self.remove_with(&id, !keep_data).await {
    Ok(()) => { tracing::info!("自动清扫已完成任务: {id}"); swept.push(id); }
    Err(e) => tracing::warn!("自动清扫 {id} 失败（跳过）: {e}"),
}
```

**这说明 `remove_with` 的契约本来就该返回真实错误，只是实现没做到。**

## 影响面

| 调用点 | 位置 | 受影响方式 |
|---|---|---|
| `DELETE /tasks/:id` | `http.rs:1494` | 返回 204，UI 认为成功 |
| `POST /tasks/batch` 的 remove | `ops.rs:2161` | 同上 |
| 自动清扫 | `ops.rs:3799` | 已有 Err 分支，但永远走不到 |
| `conflict_tests` / `list_batch_tests` | 测试 | 只测了正常路径 |

**用户可见后果**：
- 任务从 UI 消失
- `.part` 文件持续增长
- 磁盘空间无法释放
- 重启 daemon 后任务不会恢复（记录已删），但文件残留

## 建议修复

```rust
pub async fn remove_with(&self, id: &str, delete_data: bool) -> Result<(), DaemonError> {
    // 1. 先取记录（不删除）—— 失败时任务仍在列表，可重试
    let rec = self
        .tasks
        .lock()
        .get(id)
        .cloned()
        .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;

    // 2. 先停引擎，失败则传播（记录保留）
    if let Some(tid) = &rec.engine_tid {
        let engine = self
            .engine_for(rec.engine_kind)
            .map_err(|e| DaemonError::Engine(
                format!("引擎不可用，无法停止任务 {id}: {e}"))) ?;
        engine
            .remove(tid, delete_data)
            .await
            .map_err(|e| DaemonError::Engine(
                format!("停止引擎任务失败 {id}: {e}"))) ?;
    }

    // 3. 引擎确认停止后才删记录
    self.tasks.lock().remove(id);
    self.autosave();
    Ok(())
}
```

### 可选增强：立即暂停，缩短用户感知延迟

```rust
// 2a. 先 pause（立即置标志，不等段边界）
if let Ok(engine) = self.engine_for(rec.engine_kind) {
    let _ = engine.pause(tid).await;   // best-effort
}
// 2b. 再 remove
```

### 可选增强：可观测性

```rust
// 删除成功/失败都发事件，UI 可提示
hub.publish(SchedulerEvent::TaskRemoved { task_id, engine_ok });
```

## 建议补的测试

现有 `list_batch_tests.rs:161` 只测了正常路径。建议补：

```rust
/// 引擎 remove 失败时必须返回 Err，且任务记录保留（可重试）
#[tokio::test]
async fn remove_with_propagates_engine_failure_and_keeps_record() {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    fake.set_remove_result(Err(EngineError::Other("boom".into())));  // 需扩展 FakeEngine
    let state = DaemonState::new(fake.clone(), vec![]);
    let ids = add_n(&state, 1).await;

    let r = state.remove_with(&ids[0], false).await;
    assert!(r.is_err(), "引擎失败必须传播");
    assert!(state.tasks.lock().get(&ids[0]).is_some(), "任务记录必须保留以便重试");
}

/// 引擎不可用（engine_for 返回 Err）时也必须报错，不得静默跳过
#[tokio::test]
async fn remove_with_errors_when_engine_unavailable() {
    // 构造一个 engine_kind=bt 但只装配了 http 引擎的 state
    let ids = add_recovering(&state, 1).await;   // BT 任务处于 add 失败恢复态
    assert!(state.remove_with(&ids[0], false).await.is_err());
}
```

**注意**：`remove_with` 改动后需检查既有调用点：
- `http.rs:1494` 已正确处理 `Err → NOT_FOUND`，但建议区分
  `Engine` 错误为 500 而非 404
- `ops.rs:3799` 自动清扫已按 `Result` 分支，无需改动
- `list_batch_tests.rs:161` 的正常路径断言应仍然通过

---

# 问题 2：关窗后 daemon 仍在运行 —— main 已修复，建议加父进程绑定

## 0.2.1 的缺陷（已修）

`desktop/src-tauri/src/main.rs` 的代码注释如实记录：

```rust
// 审计修复（P1-1）：托盘退出必须先杀 daemon——
// AppHandle::exit 不触发窗口 CloseRequested（daemon 回收只挂在窗口
// 关闭路径上）→ 旧实现产出僵尸 daemon（占住 8788，下次启动 sidecar
// bind 失败 + 壳连到旧实例假正常）。ExitRequested 兜底另覆盖 macOS
// Cmd+Q / 系统注销路径。
```

**这正是用户 0.2.1 遇到的现象。**

## main 当前三条退出路径都覆盖了

| 路径 | 位置 | 覆盖 |
|---|---|---|
| 窗口关闭 | `main.rs:230-238` `CloseRequested` | ✅ |
| 托盘退出 | `main.rs:126-135` | ✅ |
| `ExitRequested` 兜底 | `main.rs:245-247` | ✅（含 macOS Cmd+Q / 注销） |

## 残余风险：强制杀进程 / 壳崩溃

三条路径都依赖**壳进程有机会执行 `kill_daemon`**。若：

- 用户在任务管理器强制结束进程
- `kill -9` / `taskkill /F`
- 壳自身 panic 崩溃
- 系统断电

→ `kill_daemon` 不执行 → **daemon 成孤儿继续运行**。

## 建议：把父子生命周期交给操作系统

### Windows（推荐 Job Object，最彻底）

```rust
// desktop/src-tauri/src/main.rs 或 daemon 侧
use windows_sys::Win32::System::JobObjects::*;

// 壳启动时创建 Job，设置 JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
let job = CreateJobObjectW(null(), null());
let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
SetInformationJobObject(job, JobObjectExtendedLimitInformation,
                        &info as *const _ as *const _, size_of::<...>() as u32);

// sidecar spawn 后
AssignProcessToJobObject(job, child_handle);

// → 壳进程无论怎么死（含 kill -9 / panic），OS 自动终止 daemon
```

> 项目已有 `windows-sys` 依赖（`crates/xunlei-ffi/Cargo.toml`），
> `desktop/src-tauri` 需自己加。

### Linux / macOS

```rust
// Linux: 子进程启动后立即
unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM); }
// → 父进程退出时内核发 SIGTERM

// macOS: 无等价机制，用 kqueue EVFILT_PROC 监听父 PID，
//        或 daemon 侧轮询 getppid() == 1 判定父已死
```

### 兜底的通用方案：daemon 侧自检

```rust
// daemon 启动时记录父 PID，定期检测
let parent_pid = std::os::unix::process::parent_id();  // 或 Windows 传参
tokio::spawn(async move {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tick.tick().await;
        if !process_alive(parent_pid) {
            tracing::warn!("父进程 {parent_pid} 已退出，daemon 自行停止");
            std::process::exit(0);
        }
    }
});
```

**这个方案跨平台、零外部依赖，建议作为必做项；Job Object 作为 Windows 上的加固。**

---

# 附：用户当前的临时处置

如果 0.2.1 上还有僵尸 daemon：

```powershell
# 查占用 8788 端口的进程
netstat -ano | findstr :8788
# 按 PID 强杀
taskkill /PID <pid> /F
```

删任务后确认是否真停：观察 `<dest>.part` 文件大小是否还在增长。

---

# 优先级

| # | 项 | 优先级 | 理由 |
|---|---|---|---|
| 1 | `remove_with` 顺序 + 错误传播 | **P0** | 数据/资源泄漏，用户可见，且契约本就是 `Result` |
| 2 | daemon 父进程自检 | **P1** | 强制杀进程/崩溃场景必须兜底 |
| 3 | Windows Job Object | P2 | 加固，OS 级保证 |
| 4 | remove 前先 pause | P2 | 体验优化，缩短感知延迟 |

**1 和 2 建议一起做**，因为它们都是「进程/任务本该停但没停」的同一类问题。
