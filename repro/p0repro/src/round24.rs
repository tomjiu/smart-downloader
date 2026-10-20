//! 第二十四轮：审计「删任务后仍在下载」+「关窗后进程仍在」
//!
//! 用户报告（旧版本）：
//!   1. 前端删掉任务 → 后台仍继续下载文件
//!   2. 关掉程序窗口 → daemon 仍在运行
//!
//! 本文件梳理所有相关代码路径，定位根因与残余风险。

#![allow(dead_code)]

#[cfg(test)]
mod round24 {
    #[test]
    fn analyze_remove_task_path() {
        println!("=== 问题 1：删任务后仍在下载 ===\n");

        println!("[主路径] HTTP DELETE /tasks/:id");
        println!("  http.rs:1489 remove_task");
        println!("    → state.remove_with(&id, q.delete_data)");
        println!();
        println!("[remove_with 实现] ops.rs:3631-3644");
        println!("  pub async fn remove_with(&self, id: &str, delete_data: bool) {{");
        println!("      // ① 先从 tasks 表移除记录");
        println!("      let rec = self.tasks.lock().remove(id)?;");
        println!("      // ② 再通知引擎（两条静默失败路径）");
        println!("      if let Some(tid) = rec.engine_tid {{");
        println!("          if let Ok(engine) = self.engine_for(rec.engine_kind) {{   // ← 失败静默");
        println!("              let _ = engine.remove(&tid, delete_data).await;         // ← 失败静默");
        println!("          }}");
        println!("      }}");
        println!("      self.autosave();");
        println!("  }}");
        println!();

        println!("=== 缺陷分析 ===\n");
        println!("缺陷 1：**先删记录，后停引擎**（顺序问题）");
        println!("  · ① 执行后 UI 上任务立即消失");
        println!("  · 若 ② 失败，引擎任务再无可达路径（记录已删，无法重试 remove）");
        println!("  · 结果：引擎继续下载，磁盘持续增长，UI 无任何痕迹");
        println!();
        println!("缺陷 2：`if let Ok(engine)` —— 引擎不可用时直接跳过");
        println!("  · engine_for() 返回 Err 的场景：");
        println!("    - feature 未启用（如 --features bt 未开）");
        println!("    - 引擎装配失败");
        println!("  · 此时引擎侧任务仍在跑，但没人通知它停");
        println!();
        println!("缺陷 3：`let _ = engine.remove()` —— 吞掉所有错误");
        println!("  · 引擎返回 NotFound / Other 被完全丢弃");
        println!("  · 用户与日志都看不到失败");

        println!("\n[引擎侧] HttpEngine::remove（engine.rs）");
        println!("  let t = tasks.remove(id).ok_or(EngineError::NotFound)?;");
        println!("  pause_flag.store(true, SeqCst);        // ← 只置标志");
        println!("  · worker 在**下一个段边界**才退出");
        println!("  · 段粒度 16MB（DEFAULT_MIN_SPLIT）");
        println!("  · 若正在写 16MB 段：最多再下 16MB 才停");
        println!("  · 这是**设计如此**（段边界退出是优雅停止），不是 bug");
        println!("  · 但用户感知为「删了还在下」");

        println!("\n=== 用户体验放大因素 ===\n");
        println!("  1. 删除后 UI 立即移除任务 → 用户认为已停止");
        println!("  2. 引擎侧实际要等段边界（最长 = 一个段的下载时间）");
        println!("  3. 若 remove 失败（静默）→ 永远不停");
        println!("  4. 磁盘上 .part 文件继续增长 → 用户看到文件在变大");
    }

    #[test]
    fn analyze_window_close_path() {
        println!("\n=== 问题 2：关窗后 daemon 仍在运行 ===\n");

        println!("[架构] daemon 是 sidecar 子进程（desktop/src-tauri/src/main.rs）");
        println!("  · Tauri 壳 spawn `smart-dl-daemon` 子进程");
        println!("  · 壳退出时必须显式 kill 子进程");
        println!("  · Windows 上父进程退出不会自动杀子进程");

        println!("\n[当前代码：三条退出路径都有回收]\n");
        println!("  ① 窗口关闭（on_window_event CloseRequested，main.rs:230-238）");
        println!("     api.prevent_close();");
        println!("     kill_daemon(...);");
        println!("     app.exit(0);");
        println!("     ✅ 覆盖");
        println!();
        println!("  ② 托盘退出（main.rs:126-135）");
        println!("     \"quit\" => {{ kill_daemon(...); app.exit(0); }}");
        println!("     ✅ 覆盖（审计修复 P1-1 新增）");
        println!();
        println!("  ③ ExitRequested（main.rs:245-247）");
        println!("     if let RunEvent::ExitRequested {{ .. }} = event {{");
        println!("         kill_daemon(&app.state::<DaemonChild>());");
        println!("     }}");
        println!("     ✅ 兜底（覆盖 macOS Cmd+Q / 系统注销）");

        println!("\n[代码注释明确记录了旧版本缺陷]\n");
        println!("  // 审计修复（P1-1）：托盘退出必须先杀 daemon——");
        println!("  // AppHandle::exit 不触发窗口 CloseRequested（daemon");
        println!("  // 回收只挂在窗口关闭路径上）→ 旧实现产出僵尸 daemon");
        println!("  // （占住 8788，下次启动 sidecar bind 失败 + 壳连到旧");
        println!("  // 实例假正常）。ExitRequested 兜底另覆盖 macOS Cmd+Q /");
        println!("  // 系统注销路径。");
        println!();
        println!("  → **用户用的旧版本正是这里说的「旧实现」**");
        println!("  → 已修复");

        println!("\n[残余风险]\n");
        println!("  1. 强制杀进程（任务管理器 / kill -9）：");
        println!("     · 壳被杀 → kill_daemon 不执行 → daemon 成孤儿");
        println!("     · Windows 上 daemon 会继续运行");
        println!("     · **这是当前仍存在的问题**");
        println!();
        println!("  2. 壳崩溃（panic）：");
        println!("     · 同上，daemon 成孤儿");
        println!();
        println!("  3. 已有缓解：daemon.lock 单实例锁");
        println!("     · 下次启动时若端口被占，壳会检测到");
        println!("     · 但不会主动清理旧 daemon");
    }

    #[test]
    fn recommendations() {
        println!("\n=== 修复建议 ===\n");

        println!("【问题 1：删任务后仍在下载】\n");
        println!("方案 A（推荐）：调整顺序 + 传播错误");
        println!("  pub async fn remove_with(&self, id: &str, delete_data: bool)");
        println!("      -> Result<(), DaemonError> {{");
        println!("      // 1. 先取记录（不删除）");
        println!("      let rec = self.tasks.lock().get(id).cloned()");
        println!("          .ok_or_else(|| DaemonError::NotFound(id.into()))?;");
        println!();
        println!("      // 2. 先停引擎，失败则返回错误（记录保留，可重试）");
        println!("      if let Some(tid) = &rec.engine_tid {{");
        println!("          let engine = self.engine_for(rec.engine_kind)");
        println!("              .map_err(|e| DaemonError::Engine(");
        println!("                  format!(\"引擎不可用，无法停止任务: {{e}}\")))?;");
        println!("          engine.remove(tid, delete_data).await");
        println!("              .map_err(|e| DaemonError::Engine(");
        println!("                  format!(\"停止引擎任务失败: {{e}}\")))?;");
        println!("      }}");
        println!();
        println!("      // 3. 引擎确认停止后才删记录");
        println!("      self.tasks.lock().remove(id);");
        println!("      self.autosave();");
        println!("      Ok(())");
        println!("  }}");
        println!();
        println!("  优点：");
        println!("  · 引擎失败 → UI 收到错误，任务仍在列表（用户可重试）");
        println!("  · 不会出现「UI 消失了但还在下」");

        println!("\n方案 B（补充）：删除时先 pause 再 remove");
        println!("  · 立即置 pause 标志（不等段边界）");
        println!("  · 然后 remove");
        println!("  · 用户感知更快");

        println!("\n方案 C（可观测性）：删除时发事件");
        println!("  hub.publish(SchedulerEvent::TaskRemoved {{ task_id, engine_ok }});");

        println!("\n【问题 2：关窗后 daemon 仍在】\n");
        println!("当前代码已修复（三条退出路径 + ExitRequested 兜底）。");
        println!();
        println!("残余风险（强制杀进程 / 壳崩溃）：");
        println!("  建议：daemon 侧加「父进程存活检测」");
        println!("  · Windows: 定期检查父 PID 是否还在（OpenProcess）");
        println!("  · 或用 Job Object 绑定父子生命周期（最彻底）");
        println!("  · Linux: prctl(PR_SET_PDEATHSIG, SIGTERM)");
        println!("  · 这样即使壳被 kill -9，daemon 也会自杀");

        println!("\n【立即验证方法】\n");
        println!("  1. 删除正在下载的任务 → 观察 .part 文件大小是否停止增长");
        println!("     · 若继续增长 → remove_with 的静默失败被触发");
        println!("  2. 关窗后 tasklist / ps 查 smart-dl-daemon 是否还在");
        println!("     · 若在 → 用的是旧版本或有强制杀进程");
    }
}
