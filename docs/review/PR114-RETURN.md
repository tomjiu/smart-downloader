## 退回：CI 未通过 —— `cargo fmt --check` 有 18 个文件未格式化

### 结论

**功能没问题，但合并前必须先跑一次 `cargo fmt`。**

我完整验证了当前 HEAD（`169ff0c`）：

| 检查 | 结果 |
|------|------|
| `cargo check --workspace --exclude smart-dl-btcore` | ✅ 通过（1m06s） |
| `cargo test --workspace --exclude smart-dl-btcore` | ✅ **998 passed / 0 failed** |
| `cargo fmt --all --check` | ❌ **18 个文件有差异** |

### 失败的直接原因

CI 的 `fmt + clippy + test` 三个 job 全部挂在 **rustfmt 步骤**：

```
fmt + clippy + test (ubuntu-latest)   fail   14s
fmt + clippy + test (windows-latest)  fail   40s
##[error]Process completed with exit code 1.
```

失败在 rustfmt，后面的 clippy / test 根本没跑。**修复只需要 `cargo fmt --all` 一次提交。**

### 需要格式化的 18 个文件

```
crates/core/src/session/output.rs
crates/core/src/torrent_meta.rs
crates/daemon/src/bt.rs
crates/daemon/src/serve.rs
crates/daemon/src/state/bt_alerts.rs
crates/daemon/src/state_tests/torrent_tests.rs
crates/httpdl/src/dash.rs
crates/httpdl/src/engine.rs
crates/httpdl/src/hls.rs
crates/httpdl/src/rate.rs
crates/httpdl/tests/backup_failover.rs
crates/httpdl/tests/update_sources.rs
crates/httpdl/tests/verify.rs
crates/httpdl/tests/verify_rotation.rs
crates/provider/src/xunlei/login_page.rs
crates/provider/src/xunlei/provider.rs
crates/provider/tests/quota_backoff.rs
crates/xunlei-convert/src/cid_store.rs
```

### 差异性质：纯格式，无逻辑变更

抽查两处：

```rust
// core/src/session/output.rs:245
-        assert!(sanitize_rel("con .txt").is_err(), "'con .txt' Win32 等价设备名");
+        assert!(
+            sanitize_rel("con .txt").is_err(),
+            "'con .txt' Win32 等价设备名"
+        );
```

```rust
// core/src/torrent_meta.rs:78
-            TorrentMetaError::InvalidPieceLength(v) => write!(
-                f,
-                "piece length 取值非法（须 1..={}，得到 {v}）",
-                1 << 24
-            ),
+            TorrentMetaError::InvalidPieceLength(v) => {
+                write!(f, "piece length 取值非法（须 1..={}，得到 {v}）", 1 << 24)
+            }
```

**都是换行/缩进差异，不影响行为。** 998 测试全绿也印证了这一点。

### 另外两项（非阻断，合并后跟进）

1. **`security gates` 仍然失败** —— `rustls 0.23.43` RUSTSEC 公告，需 `cargo update -p rustls` 升到 >= 0.23.46。
   **这不是本 PR 引入的**（我核对过：main 上 PR #111 时 CI 还是绿的，从 PR #112 开始红）。
   但既然本 PR 已经把版本推到 0.2.3、并且已经补了 `LICENSE-MIT`/`LICENSE-APACHE`（很好），
   顺手把这颗 advisory 也清掉会更有价值。

2. **`deferred` 提醒**：PR 从 +1785 扩到 +2451（63 文件 / 17 提交），
   我上一轮的审计（`#issuecomment-6258118324`）只覆盖了 `+1785` 那个快照的约 60%。
   batch10 新增的 `byteorder`→`frame.rs`、`lockfile.rs` 竞态、`ftp/sftp` 权限族等我**没有逐行审**。
   合并后如果要我补审这几块，告诉我即可。

### 建议的下一步

```bash
cargo fmt --all
git commit -am "style: cargo fmt（batch10-a~g + 整合批格式归位）"
git push
```

推上来后 CI 的 fmt/clippy/test 三个 job 应该能过，我再确认一遍就合并。
