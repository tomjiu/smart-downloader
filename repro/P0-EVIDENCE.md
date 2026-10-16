# P0 复现证据：多文件 .torrent 解析除零 panic

**状态：已实证复现**（2026-10-16）

## 复现方式

```powershell
cd repro\p0repro
cargo test -- --nocapture
```

## 实际输出

```
running 2 tests
=== 对照组: 非法键序（现有测试口径）===
成功: files=2
  path=a piece_offset=0 piece_count=1
  path=b piece_offset=0 piece_count=1
test p0::control_illegal_order ... ok

=== 复现 P0: 真实键序多文件 torrent ===
字节: d4:infod5:filesld6:lengthi10e4:pathl1:aeed6:lengthi20e4:pathl1:beee4:name4:mult12:piece lengthi16384e6:pieces20:                    ee
→ TorrentMeta::parse()  [piece_length 此时仍为 0]

thread 'p0::p0_divide_by_zero' panicked at src\lib.rs:239:32:
attempt to divide by zero
test p0::p0_divide_by_zero ... FAILED

test result: FAILED. 1 passed; 1 failed
```

## 关键结论

**同一份代码，两个测试给出相反结果** —— 差异仅在 bencode 键序：

| 测试 | 键序 | 结果 |
|------|------|------|
| `control_illegal_order` | `piece length` → `pieces` → `name` → `files` | ✅ 通过 |
| `p0_divide_by_zero` | **`files` → `name` → `piece length` → `pieces`** | 💥 panic |

**第二种是 bencode 规范要求的字典序**，也是 mktorrent / libtorrent / Transmission 等所有主流生成器的实际输出。

## 根因

`crates/daemon/src/state/bt_alerts.rs`：

- **L117**：`files = parse_file_list(&b[i + 1..list_end], piece_length)?;`
  → 扫到 `files` 键时立即解析，此刻 `piece_length` 仍为初值 **0**
- **L168**：`let plen = piece_length as u64;` → `0`
- **L215**：`let piece_offset = (total_size / plen) as usize;` → **除零 panic**

`piece_length` 只在扫到 `piece length` 键时赋值，而该检查（L128）在循环**之后**才执行。

## 为什么 1180 个测试没抓到

`crates/daemon/src/state_tests/torrent_tests.rs:90-112` 的 `parse_multi_file` fixture：

```rust
torrent.extend_from_slice(b"12:piece lengthi16384e"); // piece length  ← 提前
torrent.extend_from_slice(b"6:pieces20:");
torrent.extend_from_slice(&[0u8; 20]);
torrent.extend_from_slice(b"4:name8:multidir");
torrent.extend_from_slice(b"5:filesl");               // files  ← 滞后
```

**手工把 `piece length` 写在 `files` 之前**，与真实 torrent 键序相反 → `plen` 被正确赋值 → 测试恒绿。

## 影响

1. **panic**：`add` 路径同步可达（`add_torrent_task_opts` → `torrent_precheck_total` → `TorrentMeta::parse`），导入任何真实多文件 .torrent 即崩溃
2. **静默损坏**：即便某些键序下不 panic，`piece_offset`/`piece_count` 全为 0 → `ops.rs:1205` 的 `&meta.pieces_hash[file.piece_offset..file.piece_offset + file.piece_count]` 取空切片 → 完成位图归零

## 修复方向

将 `parse_file_list` 的调用与 piece 偏移计算**延后到扫描结束**：

1. 扫描阶段只收集原始 `{path, size}`
2. `piece_length` 确定后再统一计算 `piece_offset` / `piece_count`
3. 补一个**按字典序写键**的多文件 fixture（这是关键 —— 否则修了也会再退化）
