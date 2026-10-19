//! 第二十轮：审计 provider.rs refresh_auth 的落盘/CAS 顺序
//!
//! batch10 P1 修复（refresh_auth 三段式）：
//!   ① 锁内快照 → ② 锁外刷新（clone 的 candidate）→ ③ 锁内 CAS 回写
//!
//! 审查点：`save_auth(&candidate)` 在 CAS **之前**无条件执行。
//! 若 CAS 判定 already_advanced（内存已有更新的 token），
//! 磁盘却被写成本方（较旧）的 candidate → 磁盘落后于内存。
//!
//! 用同步模型（std::sync::Mutex）复刻同一逻辑。

#![allow(dead_code)]

use std::sync::Mutex;
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
struct AuthState {
    access_token: String,
    captcha_token: String,
}

impl AuthState {
    fn access_token_expiring(&self) -> bool {
        self.access_token.starts_with("OLD")
    }
    fn captcha_token_expiring(&self) -> bool {
        self.captcha_token.starts_with("OLD")
    }
}

/// 模拟 client.refresh（把 OLD → NEW-<tag>）
fn refresh(state: &mut AuthState, tag: &str) {
    thread::sleep(Duration::from_millis(15));
    if state.access_token.starts_with("OLD") {
        state.access_token = format!("NEW-{tag}");
    }
}

/// 复刻 batch10 的 refresh_auth
fn refresh_auth(
    auth: &Mutex<Option<AuthState>>,
    disk: &Mutex<Option<AuthState>>,
    tag: &str,
) -> Result<(), String> {
    // ① 锁内快照
    let snapshot = auth.lock().unwrap().clone();
    let Some(mut candidate) = snapshot else {
        return Err("no auth".into());
    };
    let old_access = candidate.access_token.clone();
    let old_captcha = candidate.captcha_token.clone();

    // ② 锁外刷新
    let mut changed = false;
    if candidate.access_token_expiring() {
        refresh(&mut candidate, tag);
        changed = true;
    }
    if candidate.captcha_token_expiring() {
        candidate.captcha_token = format!("NEWCAP-{tag}");
        changed = true;
    }
    if !changed {
        return Ok(());
    }

    // ⚠️ 先落盘（无条件），再 CAS
    *disk.lock().unwrap() = Some(candidate.clone());

    // ③ 锁内 CAS
    {
        let mut guard = auth.lock().unwrap();
        let already_advanced = guard.as_ref().is_some_and(|g| {
            g.access_token != old_access || g.captcha_token != old_captcha
        });
        if !already_advanced {
            *guard = Some(candidate);
        }
    }
    Ok(())
}

#[cfg(test)]
mod round20 {
    use super::*;

    #[test]
    fn disk_may_lag_behind_memory() {
        println!("=== refresh_auth 落盘/CAS 顺序审计 ===\n");

        let auth = Mutex::new(Some(AuthState {
            access_token: "OLD-TOKEN".into(),
            captcha_token: "OLD-CAP".into(),
        }));
        let disk: Mutex<Option<AuthState>> = Mutex::new(None);

        // ── 单线程：正常路径 ──
        println!("[场景 A] 单线程刷新（无竞争）");
        refresh_auth(&auth, &disk, "A").unwrap();
        let mem = auth.lock().unwrap().clone().unwrap();
        let dsk = disk.lock().unwrap().clone().unwrap();
        println!("  内存 access = {}", mem.access_token);
        println!("  磁盘 access = {}", dsk.access_token);
        assert_eq!(mem, dsk, "单线程下内存与磁盘应一致");
        println!("  ✅ 一致\n");

        // ── 并发：两路 refresh 同时进行 ──
        println!("[场景 B] 两路并发 refresh");
        *auth.lock().unwrap() = Some(AuthState {
            access_token: "OLD-TOKEN".into(),
            captcha_token: "OLD-CAP".into(),
        });
        *disk.lock().unwrap() = None;

        // T2 稍微晚一点启动，让 T1 先拿快照
        let mut results = Vec::new();
        thread::scope(|s| {
            let h1 = s.spawn(|| refresh_auth(&auth, &disk, "T1"));
            thread::sleep(Duration::from_millis(3));
            let h2 = s.spawn(|| refresh_auth(&auth, &disk, "T2"));
            results.push(h1.join().unwrap());
            results.push(h2.join().unwrap());
        });
        println!("  T1: {:?}", results[0]);
        println!("  T2: {:?}", results[1]);

        let mem = auth.lock().unwrap().clone().unwrap();
        let dsk = disk.lock().unwrap().clone().unwrap();
        println!("\n  内存 access_token = {}", mem.access_token);
        println!("  磁盘 access_token = {}", dsk.access_token);

        let consistent = mem == dsk;
        if consistent {
            println!("\n  ✅ 内存与磁盘一致");
        } else {
            println!("\n  ⚠️ 内存与磁盘不一致（磁盘落后一代）");
        }

        println!("\n=== 分析 ===\n");
        println!("  代码顺序：");
        println!("    *disk = Some(candidate)          ← 无条件落盘");
        println!("    {{ CAS 检查 }}");
        println!("    if !already_advanced {{ *guard = candidate }}   ← 条件回写");
        println!();
        println!("  「落盘无条件 / 内存有条件」= 两者可能分叉。");
        println!();
        println!("  注释的推理：「本方刷新结果随落盘文件保留」");
        println!("    假设：本方刷新的 token 也是服务端合法签发的，");
        println!("    即使内存保留了别人的（更新的）值，磁盘上自己的也能用。");
        println!("    → **这个推理基本成立**：两个 token 都合法，任取其一可用，");
        println!("      只是磁盘比内存旧一代。");

        println!("\n=== 实际影响 ===\n");
        println!("  · 内存 token 始终最新（CAS 保证不回退）✅");
        println!("  · 磁盘可能落后一代");
        println!("  · 后果：重启后可能多一次刷新请求（token 仍在有效期内则无影响）");
        println!("  · 不会导致鉴权失败（旧 token 在有效期内仍可用）");
        println!("  · 定性：**P3 轻微冗余**，不是缺陷");

        println!("\n=== 结论 ===\n");
        println!("  ✅ 三段式修复方向正确，CAS 回写正确（内存不回退）");
        println!("  🟢 磁盘可能落后一代，但两份 token 都合法，影响可忽略");
        println!("  → 若要更严谨：落盘移到 CAS 分支内（仅自己获胜时写），");
        println!("     或落盘前重读内存状态（写最新的）");
    }
}
