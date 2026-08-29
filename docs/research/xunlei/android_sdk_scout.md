# Android 版迅雷下载引擎（libxl_thunder_sdk.so）侦察报告

2026-08-29，来自 `x-player-guanwang.apk`（迅雷 Android 客户端 80.7MB）。

## 包结构

APK 内 `lib/arm64-v8a/` 和 `lib/armeabi-v7a/` 各 21 个 .so，其中与下载引擎相关的：

| .so | arm64 大小 | 作用 |
|-----|-----------|------|
| `libxl_thunder_sdk.so` | 8,961,424 | **迅雷下载引擎核心（DownloadLib）** |
| `libscrape.so` | 39,710,080 | 抓取/解析引擎（最大，39.7MB） |
| `libxl_video_control.so` | 683,408 | 视频控制（HLS/VOD） |
| `libaplayer.so` | 19,445,944 | 播放器 |

## libxl_thunder_sdk.so 关键特征

- **架构**：ELF64, AArch64 (ARM64)
- **未加壳**：只有 1 个 ELF magic，无 UPX/加固特征
- **符号表被完全剥离**：`.dynamic` 段只有 NEEDED(6) + SONAME，**无 SYMTAB/STRTAB**，无 GNU_HASH
- **结论**：不是给外部 dlopen 的 SDK，是客户端内部 JNI 直接链接的私有引擎

## 完整 API 面（从字符串表还原，159 个函数）

核心是 C++ 类 `xldownloadlib::DownloadLib`，函数命名**驼峰**（与 Windows 逆向的 `XL_CreateBTTask_V2` 下划线风格**不同**）：

### 任务创建（比 Windows 版多得多）
- `XLCreateBtTask` / `XLCreateBtMagnetTask` — BT/磁力 ✅（我们最关心的）
- `XLCreateP2spTask` — P2SP
- `XLCreateEmuleTask` — 电驴
- `XLCreateHLSTask` / `XLCreateVodTask` — 流媒体
- `XLCreateCDNTask` / `XLCreateCIDTask` / `XLCreateFuzzyTask` / `XLCreateShortVideoTask` / `XLCreateAdaptiveTask`

### 查询（含速度！）
- `XLGetTaskInfo` / `XLGetTaskInfoEx` / `XLGetBatchTaskInfo` / `XLGetBatchTaskStatus`
- `XLGetBtSubTaskInfo` / `XLGetBtSubTaskStatus` / `XLGetTorrentInfo`
- `XLGetGlobalDownloadSpeed` ✅（Windows 版缺的速度查询，这里有！）
- `XLGetUploadBriefInfo` / `XLGetUploadInfo` / `XLGetUploadFileInfoList`

### 控制
- `XLStartTask` / `XLStartTask2` / `XLStopTask` / `XLStopTaskWithReason`
- `XLReleaseTask`（注意：是"释放"不是 Windows 的 DeleteTask）
- `XLSetSpeedLimit` / `XLSetTaskSpeedLimit` / `XLGetEstimateBandWidth`

### 网络/DHT
- `XLInsertDHTNode` ✅（DHT 节点注入，Windows 版未确认）
- `XLAddBtTrackerNodes` / `XLAddPeerResource` / `XLAddBtRecord`
- `XLChangeDomainToIP` / `XLSetLocalHostResolve`

### 身份/回调
- `XLSetUserId` / `XLSetVipType` / `XLSetUserAgent`
- `XLSetTaskStatusCallback` / `XLSetPeerVerifyCallback`

## 与 Windows 版（DownloadSDKProxy.dll）的关键差异

| 维度 | Windows（已逆向） | Android（本次） |
|------|------------------|----------------|
| 函数命名 | `XL_CreateBTTask_V2`（下划线+_V2） | `XLCreateBtTask`（驼峰） |
| 符号导出 | 100 个导出（可 dlopen） | 符号剥离（JNI 内部） |
| 引擎类 | `DownloadSDK.dll`（server 进程） | `DownloadLib`（进程内） |
| 速度查询 | ❌ 缺（需逆向 QueryTaskFlow） | ✅ `XLGetGlobalDownloadSpeed` |
| 任务删除 | `XL_DeleteTask` | `XLReleaseTask` |
| BT 能力 | ✅ 已真机验证 | ✅ 有 XLCreateBtTask/XLCreateBtMagnetTask |

## 战略判断

**价值**：Android 版 API 面更完整（159 vs 100），含速度查询、DHT 注入、流媒体等 Windows 版缺的能力。命名更"官方"（DownloadLib 类），暗示这是迅雷**移动开放 SDK**的形态。

**障碍**：
1. 符号表被剥离，无法直接 dlopen 调用——需要先逆向 JNI 层（classes.dex）找到 native 方法注册表，或从字符串+反汇编重建函数地址
2. ARM64 反汇编（capstone ARM64 模式）是新工作，之前全是 x86-64
3. 这是**移动端** SDK，目标平台是 Android，与我们 Rust 桌面端的交叉平台价值有限（除非走 Android NDK 交叉编译）

**建议**：暂不做 Android .so 的 ABI 逆向（成本高、平台错位）。更值得先看 **macOS DMG** 里的 .dylib——那才是桌面端、与 Rust 工具链同平台、可能直接复用的目标。
