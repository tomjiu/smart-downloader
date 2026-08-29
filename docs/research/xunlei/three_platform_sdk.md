# 迅雷下载引擎三平台画像（决定性发现）

2026-08-29，来自用户提供的两个安装包：
- `x-player-guanwang.apk`（迅雷 Android 80.7MB）
- `thunder_5.80.7.66659.dmg`（迅雷 macOS 114MB）

## 核心结论

迅雷的下载引擎核心是一个跨平台的 C++ 类 **`xldownloadlib::DownloadLib`**，
对外暴露两套命名风格完全一致的 C API（`XLCreate*` 驼峰命名）。

三平台完整画像：

| 平台 | 引擎二进制 | 大小 | 导出符号 | 可 dlopen | 命名风格 |
|------|-----------|------|---------|----------|---------|
| Windows | `DownloadSDKProxy.dll`（薄代理）+ `DownloadSDK.dll`（server） | ~4.7MB | 100 个 `XL_*` 下划线 | ✅（已逆向） | `XL_CreateBTTask_V2` |
| Android | `libxl_thunder_sdk.so`（lib/arm64-v8a + armeabi-v7a） | 8.96MB | 符号剥离 | ❌（JNI 内部） | `XLCreateBtTask`（字符串残留） |
| **macOS** | **`DownloadKit.framework`（Versions/A/DownloadKit）** | **22.1MB（双架构）** | **153 个 `XL*` C 导出，完整未剥离** | **✅✅ 最友好** | `XLCreateBtTask` + `XL_InitDownloadLib` |

## macOS DownloadKit.framework 详情（决定性）

- **位置**：`Thunder.app/Contents/Bundles/XLEmbeddedPlayer.app/Contents/Frameworks/DownloadKit.framework/Versions/A/DownloadKit`
- **架构**：FAT 通用二进制（x86_64 @ 0x4000 + arm64 @ 0xb5c000），已提取 arm64（11.26MB）
- **符号表**：94892 个符号，17109 个 external，**5472 个 C 风格导出**（未剥离！）
- **153 个 `XL` 开头 C 导出函数**

### 关键 C 导出（可直接 dlopen）
**生命周期**：`XL_InitDownloadLib`、`XL_UnInitDownloadLib`、`XLInit`、`XLUnInit`
**任务创建**（11 种）：
- `XLCreateBtTask`（BT）、`XLCreateBtMagnetTask`（磁力）、`XLCreateP2spTask`（P2SP）
- `XLCreateEmuleTask`（电驴）、`XLCreateHLSTask`（HLS）、`XLCreateVodTask`（点播）
- `XLCreateCDNTask`、`XLCreateCIDTask`、`XLCreateFuzzyTask`、`XLCreateSFTask`、`XLCreateShortVideoTask`
**查询**：
- `XLGetTaskInfo`、`XLGetTaskInfoEx`、`XLGetBtSubTaskInfo`、`XLGetBtSubTaskStatus`
- `XLGetGlobalDownloadSpeed`（速度！）、`XLGetStateInfo`、`XLGetTorrentInfo`
- `XL_GetTaskId`、`XL_GetTaskInfo`、`XL_GetError`、`XL_GetInterfaceVersion`（下划线版 API）
**控制**：`XLStartTask`、`XLStartTask2`、`XLStopTask`、`XLStopTaskWithReason`、`XLReleaseTask`
**网络/DHT**：`XLInsertDHTNode`、`XLAddBtTrackerNodes`、`XLAddPeerResource`、`XLAddServerResource`
**身份/回调**：`XLSetUserId`、`XLSetVipType`、`XLSetTaskStatusCallback`、`XLSetPeerVerifyCallback`、`XLSetMsgCallback`
**视频**：`XL_CreateVideoControler`、`XL_VideoCtrlInit`、`XL_LaunchPlayTask`（边下边播）

### C++ 类 DownloadLib 方法（mangled 名，含完整类型签名）
```
DownloadLib::CreateBtTask(TAG_TASK_PARAM_BT*, unsigned long long*)  // task_id 是 u64！
DownloadLib::CreateP2spTask(TAG_TASK_PARAM*, unsigned long long*)
DownloadLib::CreateEmuleTask(TAG_TASK_PARAM_EMULE*, unsigned long long*)
DownloadLib::CreateBtMagnetTask(TAG_TASK_PARAM_MAGNET*, unsigned long long*)
DownloadLib::GetTaskInfo(unsigned long long, TAG_XL_TASK_INFO_EX*)
DownloadLib::GetTaskInfoEx(unsigned long long, TAG_XL_TASK_INFO_EEX*)
DownloadLib::ReleaseTask(unsigned long long)
DownloadLib::SetTaskUid(unsigned long long, unsigned int)
```

### 关键类型名（从符号名还原）
- `TAG_TASK_PARAM_BT` — BT 任务参数
- `TAG_TASK_PARAM_MAGNET` — 磁力任务参数
- `TAG_TASK_PARAM` — P2SP 通用任务参数
- `TAG_TASK_PARAM_EMULE` — 电驴参数
- `TAG_XL_TASK_INFO_EX` / `TAG_XL_TASK_INFO_EEX` — 任务信息（Ex/EEX 两个版本）
- `TAG_BT_SUBTASK_DETAIL` — BT 子任务详情
- `TAG_TORRENT_INFO` — torrent 信息
- `_P2PExternalResource` — P2P 外部资源

## 与 Windows 版（已逆向）的差异

| 维度 | Windows（XL_CreateBTTask_V2） | macOS（XLCreateBtTask） |
|------|------------------------------|------------------------|
| task_id 类型 | u32（`mov esi, ecx`） | **u64**（`unsigned long long`） |
| 结构体命名 | `XLBTTaskParamV2` | `TAG_TASK_PARAM_BT` |
| 函数命名 | 下划线 + `_V2` 后缀 | 驼峰，无后缀 |
| 符号导出 | 100 个（需逆向语义） | 153 个 + 完整 C++ mangled 签名 |
| 删除 | `XL_DeleteTask` | `XLReleaseTask` |
| 速度查询 | 需逆向 QueryTaskFlow | `XLGetGlobalDownloadSpeed` |
| 视频边下边播 | ❌ | ✅ `XL_LaunchPlayTask` |

## 战略意义

1. **macOS 版是最友好的逆向/集成目标**：符号完整未剥离，C++ mangled 名直接给出**完整类型签名**（参数类型、结构体名），无需像 Windows 那样逐个反汇编猜布局。

2. **可交叉验证**：Windows 版逆向出的 ABI（task_id、结构体布局、错误码）可以与 macOS 版的 mangled 签名**互相印证**，大幅降低逆向不确定性。

3. **跨平台 BT/磁力成为可能**：如果有 macOS 版 `XLCreateBtTask` 的完整签名 + 结构体布局，就能在 macOS（甚至通过 arm64/x86_64 交叉）复用迅雷 BT 下载引擎，补齐"BT 仅 Windows"的空缺。

4. **task_id 类型差异需注意**：macOS 是 u64，Windows 逆向是 u32——说明两套 SDK 版本不同（macOS 更新），不能直接套用 Windows 的 ABI。

## 下一步建议

1. **dump macOS 版 `TAG_TASK_PARAM_BT` 等结构体布局**：用 mangled 签名 + DWARF（若未剥离调试信息）+ 反汇编，还原结构体字段——比 Windows 逆向容易得多。
2. **还原 C 导出函数的完整签名**：153 个 `XL*` C 函数的参数/返回值，可结合 C++ mangled 名推导。
3. **决定集成目标**：是否新起 `xunlei-dlsdk` crate（macOS 优先，复用 DownloadKit.framework）。
