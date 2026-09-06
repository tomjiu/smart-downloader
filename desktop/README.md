# 桌面端（Tauri v2 壳）

原生窗口 + 托盘 + daemon sidecar。架构：**daemon 自身通过 `--ui-dir` 服务内嵌
UI（S2），桌面壳只负责拉起进程 + 开窗指向 `http://127.0.0.1:<port>`**——
同一份 `ui/out` 资源同时服务浏览器模式与桌面模式，零 CORS、零重复打包。

```
┌─ Smart Downloader.app / .exe ─────────────────────────┐
│  Tauri 壳（本目录）                                    │
│   ├─ 窗口 ──► http://127.0.0.1:8788  (daemon ServeDir) │
│   ├─ 托盘：显示 / 退出（回收 daemon）                   │
│   └─ sidecar: smart-dl-daemon serve --ui-dir <res>/ui  │
└────────────────────────────────────────────────────────┘
```

## 本地构建

前置：Rust 1.77+；Linux 需 webkit2gtk-4.1 系统库
（`apt install libwebkit2gtk-4.1-dev build-essential`）；
Windows 需 MSVC + WebView2；macOS 需 Xcode CLT。

```bash
# 1. 构建前端静态产物
cd ui && bun install && bun run build && cd ..

# 2. 构建 daemon（桌面版默认不含 BT——原生 libtorrent 依赖按平台另行装配；
#    Windows 可用 vcpkg libtorrent 后追加 --features bt）
cargo build --release -p smart-dl-daemon

# 3. 放置 sidecar（必须带 target-triple 后缀）
mkdir -p desktop/binaries
cp target/release/smart-dl-daemon \
   desktop/binaries/smart-dl-daemon-x86_64-unknown-linux-gnu   # 按平台调整

# 4. 打包（tauri-cli v2）
cargo install tauri-cli --version '^2'
cargo tauri build                # 产物在 desktop/src-tauri/target/release/bundle/
# 或开发调试：
cargo tauri dev
```

端口默认 8788；`SMART_DL_DESKTOP_PORT=9000 cargo tauri dev` 可换端口
（避免与常驻 daemon 冲突）。

## CI 打包

`.github/workflows/desktop.yml`：三平台（linux/windows/macos）矩阵，
自动构建 ui → daemon → tauri bundle，产物（deb/AppImage/msi/nsis/
dmg/app）挂到 GitHub Release。tag `desktop-v*` 触发。

## 边界与后续

- 桌面版 daemon 当前不含 BT 引擎（避免三平台原生 libtorrent 打包矩阵）；
  后续按平台补（Windows vcpkg / Linux apt 前缀 / macOS brew）。
- 开机自启、协议注册（magnet:）、单实例锁为后续迭代项。
