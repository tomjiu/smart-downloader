# BT 构建本地门槛（Linux）

bt 集成测试需要 libtorrent native 环境（FFI 内核静态库 + e2e seeder）。
CI 已自动化（`bt integration` job），本地开发者用同一脚本一步到位——
**无需手动装 libtorrent 开发包、无需 root**。

## 快速开始

```bash
# 1) 生成 native 环境（产物落 ~/bt-native；可重复执行覆盖）
bash scripts/ci/bt-linux-setup.sh --no-root "$HOME/bt-native"

# 2) 加载环境快照
source "$HOME/bt-native/env.sh"

# 3) 跑 BT 面测试
cargo test -p smart-dl-btcore
cargo test -p smart-dl-daemon --features bt
```

rootful 环境（如开发机可直接 sudo）去掉 `--no-root` 即可，其余步骤相同。

## 脚本做了什么

| 产物 | 用途 | 环境变量 |
|---|---|---|
| `lib/liblt_kernel.a` | FFI 内核静态库 | `LT_KERNEL_LIB_DIR` |
| `lib/seed_main` | e2e seeder 可执行 | `SEED_MAIN` |
| `fakevcpkg/` | 仿真 build.rs 的 vcpkg 契约（`*.lib` 别名 + `lib*.so` 实链） | `LT_VCPKG_LIB_DIR` |
| `prefix/` | libtorrent 头文件与动态库本地前缀（仅 no-root） | `LD_LIBRARY_PATH` |
| `linker-wrap.sh` | rustc 1.98 链接布局规避（见下） | `CARGO_TARGET_..._LINKER` |
| `env.sh` | 以上环境变量快照 | — |

- 版本兼容：Linux 发行版 libtorrent 均为 2.0.x（Debian trixie 2.0.11 /
  Ubuntu noble 2.0.10），`ffi/src` 的 2.1 API 引用由源内
  `#if LIBTORRENT_VERSION_NUM >= 20100` 编译期守卫解决，2.0.x 走回退分支。
- **rustc 1.98 链接坑**（no-root 本地前缀场景实测必踩，rootful 系统路径
  不受影响）：native `-L` 被排在 `-l` 之后（ld 单遍扫描 → unable to find
  library）+ `-fuse-ld=lld`（系统无 `ld.lld` → collect2 cannot find 'ld'）
  + `-nodefaultlibs`（缺 libstdc++ DSO 闭包）。`linker-wrap.sh` 以 g++
  驱动统一规避：剥 `-fuse-ld=lld`/`-B`/`-nodefaultlibs`，并把本地前缀
  两个 `-L` 注入首个 `-l` 之前。env.sh 已把链接器指向该 wrapper，无需手工干预。

## 常见问题

- **`cargo: command not found`**：env.sh 不含 PATH，先
  `export PATH="$HOME/.cargo/bin:$PATH"` 再 source。
- **磁盘受限**：bt 构建构件较大，建议 `CARGO_PROFILE_DEV_DEBUG=0`
  （构件体积约 -60%）；超限用 `cargo clean -p <包>` 定向回收。
- **测试文件端口互撞（6881）**：bt 面测试需按 lt_gate 串行门执行
  （`tests/common/lt_gate.rs`，插入工具 `scripts/insert_lt_gate.py`
  幂等维护）——正常 `cargo test` 无感，勿用 nextest（跨进程门未实现）。
- **Windows/macOS**：本脚本仅 Linux；Windows 走 vcpkg 原生链路
  （`ffi/vcpkg.json`），macOS 待逆向验证（TAG_XL_TASK_INFO_EX 相关，见 PROJECT_STATUS）。
