# macOS DownloadKit SDK ABI 逆向 — 阶段性成果与待办

2026-08-29，基于 `thunder_5.80.7.66659.dmg` 提取的
`DownloadKit.framework/Versions/A/DownloadKit`（arm64 slice）。

---

## 已还原的结构体布局（反汇编铁证）

### 1. TAG_TASK_PARAM_BT（BT 任务参数）

来源：`_XLCreateBtTask`（0x594390）反汇编。

```
+0x00: u32  field0   （ldp w11, w12, [x20] 读两个 u32）
+0x04: u32  field4
+0x08: u32  field8   （ldr w13, [x20, #8]）
+0x0c: （padding？未访问）
+0x10: ptr  str1     （ldr x1, [x20, #0x10]）— 窄字符串指针
+0x18: u32  str1_len （ldr w2, [x20, #0x18]）— 字符串长度
+0x20: ptr  str2     （ldr x1, [x20, #0x20]）— 窄字符串指针
+0x28: u32  str2_len （ldr w2, [x20, #0x28]）— 字符串长度
...
+0x48: u32  flag     （ldr w8, [x0, #0x48]; cbz → 返回 0x238e 错误）
```

**关键判断**：
- `+0x10/+0x18` 和 `+0x20/+0x28` 是两对 `(const char* ptr, u32 len)` 字符串
  —— 大概率是 **torrent 路径** + **保存路径**（C API 用窄字符串 + 显式长度）
- `+0x48` 是**必需的非空标志**（`cbz w8 → 返回错误 0x238e`），
  对应 Windows 版 `XLBTTaskParamV2.third_str` 必须非空的铁证！
- 错误码 `0x238e` = 十进制 9102（参数错误类）

### 2. XLInitParam（初始化参数结构体）

来源：`_XL_InitDownloadLib`（0x5f86ac）C 边界反汇编。

```c
struct XLInitParam {
    u64  field_0;      // +0x00
    u32  field_8;      // +0x08
    u64  field_10;     // +0x10
    u32  field_18;     // +0x18
    u64  field_20;     // +0x20
    u32  field_28;     // +0x28
    u64  field_30;     // +0x30
    u32  field_38;     // +0x38
    u64  field_40;     // +0x40
    u32  field_48;     // +0x48
    u64  field_50;     // +0x50
    u32  field_58;     // +0x58
    u64  field_60;     // +0x60
    u32  field_68;     // +0x68
    u64  field_70;     // +0x70
    u32  field_78;     // +0x78
    u64  field_80;     // +0x80
    f64  field_88;     // +0x88  // ldr d0, [x0, #0x88]
    u32  field_90;     // +0x90
    // 总大小：至少 0x94 = 148 字节
};
```

**模式**：前 9 组是 `(ptr: u64, len: u32)` 交替排列，+0x88 处突然出现 `f64`（可能是版本号/速度上限），末尾 +0x90 是 u32 flags。

这很可能是 **9 个配置字符串**（路径、URL、回调等）+ 一个浮点配置 + 标志位。

### 3. 与 Windows 版 XLBTTaskParamV2 的对照

| 维度 | Windows（已逆向） | macOS（本次） |
|------|------------------|--------------|
| 结构体名 | `XLBTTaskParamV2` | `TAG_TASK_PARAM_BT` |
| size | 0x28 = 40 字节 | 至少 0x4c = 76 字节 |
| 字符串编码 | 宽字符串（wcslen） | 窄字符串（char* + len） |
| 必需字段 | third_str(+0x14) 非空 | flag(+0x48) 非空 |
| task_id | u32 | **u64** |
| 命名 | 下划线 + _V2 | 驼峰 |

**结论**：两套 SDK 是**不同版本**，结构体布局**不兼容**，不能直接套用。

---

## 已确认的 C API 函数签名

### Task 生命周期

```c
i32 XLStartTask(u64 task_id, u32 param);     // 0x58e390 → 0x5160ac
i32 XLStopTask(u64 task_id, u32 param);      // 0x58e4d0 → 0x516198
i32 XLReleaseTask(u64 task_id);              // 0x58e268 → 0x515fc8
```

### 任务创建

```c
i32 XLCreateBtTask(const TAG_TASK_PARAM_BT* param, u64* out_task_id);
i32 XLCreateBtMagnetTask(const TAG_TASK_PARAM_MAGNET* param, u64* out_task_id);
i32 XLCreateP2spTask(const TAG_TASK_PARAM_P2SP* param, u64* out_task_id);
i32 XLCreateEmuleTask(const TAG_TASK_PARAM_EMULE* param, u64* out_task_id);
```

### 任务查询

```c
i32 XLGetTaskInfo(u64 task_id, TAG_XL_TASK_INFO_EX* out);
```

内部模式：DownloadLib singleton → 分配 0x90 字节对象 → 调用 0x506844 填充 → 虚函数读取数据。

### 速度查询

```c
i32 XLGetGlobalDownloadSpeed(
    XLDownloadLib lib,    // x0: DownloadLib 单例
    u64 task_id,          // x1: 任务 ID
    u64* out_speed        // 输出速度（字节/秒）
);
```

或更简单的 3 参数版本（x2 为 bool is_upload）。

### 初始化/反初始化

```c
i32 XL_InitDownloadLib(const XLInitParam* param);
i32 XL_UnInitDownloadLib();
```

---

## 关键函数地址

| 函数 | 地址（文件偏移） | 说明 |
|------|----------------|------|
| `DownloadLib::CreateBtTask` | 0x5195dc | BT 任务创建 |
| `DownloadLib::CreateBtMagnetTask` | 0x519414 | 磁力任务创建 |
| `DownloadLib::CreateP2spTask` | 0x515edc | P2SP 任务创建 |
| `DownloadLib::GetTaskInfo` | 0x5168cc | 任务信息查询 |
| `DownloadLib::StartTask` | 0x5160ac | 任务启动 |
| `DownloadLib::StopTask` | 0x516198 | 任务停止 |
| `DownloadLib::ReleaseTask` | 0x515fc8 | 任务释放 |
| `_XLCreateBtTask`（C 边界） | 0x594390 | C API 入口 |
| `_XLStartTask` | 0x58e390 | C API 入口 |
| `_XLStopTask` | 0x58e4d0 | C API 入口 |
| `_XLReleaseTask` | 0x58e268 | C API 入口 |
| `_XLGetTaskInfo` | 0x58f7f8 | C API 入口 |
| `_XLGetGlobalDownloadSpeed` | 0x596154 | C API 入口 |
| `_XL_InitDownloadLib` | 0x5f86ac | C API 入口 |
| `_XL_UnInitDownloadLib` | 0x5f8764 | C API 入口 |
| `DownloadLib` 内部构造函数 | 0x5d7c7c | 所有 Create* 共享 |
| `DownloadLib` 内部析构函数 | 0x5d7c8c | 所有 Create* 共享 |

---

## DownloadLib mangled 名函数列表（101 个）

从符号表提取的 DownloadLib 成员函数（部分解码）：

```
DownloadLib::10SetTaskUid(unsigned long long, unsigned int)
DownloadLib::10SetVipType(const void*, char, unsigned int)
DownloadLib::10SynPlayPos(unsigned long long, unsigned long long)
DownloadLib::11GetLocalUrl(const void*, char, int, const char*, int)
DownloadLib::11IsLogTurnOn(unsigned int*)
DownloadLib::11PostMessageIM11TaskManagerFibEJPS1_RbEEE13XL_ERRNO_COD(...)
DownloadLib::11PostMessageIM11TaskManagerFiyiEJPS1_RyRiEEE13XL_ERRNO_COD(...)
DownloadLib::11PostMessageIM11TaskManagerFiyjEJPS1_RyRjEEE13XL_ERRNO_COD(...)
DownloadLib::11PostMessageIM11TaskManagerFiyjjEJPS1_RyRjS6_EEE13XL_ERRNO_COD(...)
DownloadLib::11PostMessageIRFiPKcjS2_jEJS2_mS2_mEEE13XL_ERRNO_COD(...)
DownloadLib::11SendMessageIM11TaskManagerFiPKcjEJPS1_RS3_RjEEE13XL_ERRNO_COD(...)
DownloadLib::11SendMessageIM11TaskManagerFijjjEJPS1_RjS5_S5_EEE13XL_ERRNO_COD(...)
DownloadLib::11SetFileName(unsigned long long, const void*, char, unsigned int)
DownloadLib::12CreateBtTask(void*, 1, 7, T, A, G, _, T, A, ?, const , _, void*, A, R, A, M, _, B, T, const unsigned long long*)
DownloadLib::12CreateSFTask(void*, 1, 7, T, A, G, _, T, A, ?, const , _, void*, A, R, A, M, ?, F, const unsigned long long*)
DownloadLib::12GetStateInfo(void*, 9, ?, t, a, t, e, I, n, float, o)
DownloadLib::12SetHttpProxy(unsigned long long, t, const void*, char, unsigned int)
DownloadLib::12SetIndexInfoEyP24TAG_SET_IND(X, _, I, N, F, O, _, void*, A, R, A, M)
DownloadLib::12SetPipeLimit(x, x)
DownloadLib::12SetTaskToken(unsigned long long, const void*, char, unsigned int)
DownloadLib::12SynPlayStateEy17_SYN_PLAY(R, _, ?, T, A, T, E)
DownloadLib::13CreateCDNTask(void*, 1, 4, T, A, G, _, T, A, ?, const , _, void*, A, R, A, M, const unsigned long long*)
DownloadLib::13CreateCIDTask(void*, 1, 8, T, A, G, _, T, A, ?, const , _, void*, A, R, A, M, _, C, I, D, const unsigned long long*)
DownloadLib::13CreateHLSTask(void*, 1, 8, T, A, G, _, T, A, ?, const , _, void*, A, R, A, M, _, H, L, ?, const unsigned long long*)
DownloadLib::13CreateVodTask(void*, 1, 4, T, A, G, _, T, A, ?, const , _, void*, A, R, A, M, int, const unsigned long long*)
DownloadLib::13GetUploadInfo(void*, 1, 1, _, U, p, l, o, a, double, I, n, float, o)
DownloadLib::13InsertDHTNodeERKNSt3__112basic_stringIcNS0_11char_traitsIcEENS0_9allocatorIcEEE(...)
DownloadLib::13SetCAFilePath(const void*, char, ?, 1, _)
DownloadLib::13SetPlayerMode(unsigned long long, int)
DownloadLib::13SetReleaseLogEjP25TAG_SET_RELEAS(_, L, O, G, _, void*, A, R, A, M)
DownloadLib::13SetSpeedLimit(x, x)
DownloadLib::13SetUploadInfo(void*, 1, 1, _, U, p, l, o, a, double, I, n, float, o)
DownloadLib::13SynPlayCached(unsigned long long, int)
DownloadLib::14CreateP2spTask(void*, 1, 4, T, A, G, _, T, A, ?, const , _, void*, A, R, A, M, const unsigned long long*)
DownloadLib::14GetTaskAppInfo(void*, 1, 3, _, ?, D, const , _, A, void*, void*, _, I, N, F, O)
DownloadLib::14GetTorrentInfoEPKcjP16TAG_TORR(N, T, _, I, N, F, O)
DownloadLib::14SetMiUiVersion(const void*, char, unsigned int)
DownloadLib::14SetTaskCfgPath(const void*, char, unsigned int)
DownloadLib::14StopPureUpload(const void*, char, unsigned long long)
DownloadLib::14SynPlayBitrate(unsigned long long, unsigned int)
DownloadLib::15AddPeerResourceEyiP20_P2P(x, t, e, r, n, a, l, R, e, s, o, u, r, char, e)
DownloadLib::15BtSelectSubTask(unsigned long long, unsigned int*, unsigned int)
DownloadLib::15ChangeOriginRes(unsigned long long, const void*, char, int)
DownloadLib::15CreateEmuleTaskEP20TAG_TASK_PARAM_EMUL(const unsigned long long*)
DownloadLib::15CreateFuzzyTask(void*, 1, 4, T, A, G, _, T, A, ?, const , _, void*, A, R, A, M, const unsigned long long*)
DownloadLib::15GetSettingValueERKNSt3__112basic_stringIcNS0_11char_traitsIcEENS0_9allocatorIcEEE(...)
DownloadLib::15GetUrlQuickInfo(unsigned long long, void*, 1, 8, T, A, G, _, U, R, L, _, Q, U, I, C, const , _, I, N, F, O)
DownloadLib::15NotifyWifiBSSID(const void*, char, unsigned int)
DownloadLib::16GetBtSubTaskInfoEyiP21TAG_BT_SUBTASK_D(T, A, I, L)
DownloadLib::16GetTaskCheckInfoEyP22TAG_XL_TASK_CH(C, const , _, I, N, F, O)
DownloadLib::16ParserTBase64UrlEPKcjP20TAG_TBAS(6, 4, _, U, R, L, _, I, N, F, O)
DownloadLib::16SetLocalProperty(const void*, char, unsigned int, ?, 1, _, unsigned int)
DownloadLib::16StatExternalInfo(unsigned long long, int, const void*, char, ?, 1, _)
DownloadLib::17AddBtTrackerNodes(unsigned long long, const void*, char)
DownloadLib::17AddServerResourceEyiRKNSt3__112basic_stringIcNS0_11char_traitsIcEENS0_9allocatorIcEEEES8_S8_j17_RES_USE_STRAT(...)
DownloadLib::17BtDeselectSubTask(unsigned long long, unsigned int*, unsigned int)
DownloadLib::17GetPremiumResInfoEyiP23TAG_XL_PREMIUM_R(?, _, I, N, F, O)
DownloadLib::17NotifyNetWorkType(1, 2, _, N, e, t, W, o, r, k, T, unsigned long long, p, e)
DownloadLib::17SetBtPriorSubTask(unsigned long long, int)
DownloadLib::17SetTaskSocketMark(unsigned long long, int)
DownloadLib::17SetTaskSpeedLimit(unsigned long long, x)
DownloadLib::17SetTaskUidWithPid(unsigned long long, unsigned int, unsigned int)
DownloadLib::18CreateBtMagnetTaskEP21TAG_TASK_PARAM_MAGN(T, const unsigned long long*)
DownloadLib::18GetBtSubTaskStatus(unsigned long long, void*, 1, 8, T, A, G, _, B, T, _, T, A, ?, const , _, ?, T, A, T, U, ?, unsigned int, unsigned int)
DownloadLib::18GetFileNameFromUrl(const void*, char, unsigned int, const char*, unsigned int)
DownloadLib::18GetFirstMediaState(unsigned long long, int, void*, 1, 6, _, F, int, r, s, t, M, e, double, int, a, ?, t, a, t, e)
DownloadLib::18GetUploadBriefInfo(void*, 1, 6, _, U, p, l, o, a, double, B, r, int, e, float, I, n, float, o)
DownloadLib::18SetAccelerateTokenEyiP26TAG_ACCELERATE_TOK(N, _, void*, A, R, A, M)
DownloadLib::18SetOriginUserAgent(unsigned long long, const void*, char, unsigned int)
DownloadLib::19AddBatchDcdnPeerRes(unsigned long long, int, unsigned long long, void*, 2, 1, _, T, a, s, k, void*, a, r, a, m, D, char, double, n, void*, e, e, r, R, e, s, unsigned int)
DownloadLib::19GetMaxDownloadSpeed(void*, x)
DownloadLib::19GetSessionInfoByUrlERKNSt3__112basic_stringIcNS0_11char_traitsIcEENS0_9allocatorIcEEE(...)
DownloadLib::19RemoveAddedResource(unsigned long long, int, unsigned int)
DownloadLib::19SetLocalHostResolve(const void*, char, unsigned int, ?, 1, _, unsigned int)
DownloadLib::19StatExternalInfoU64(unsigned long long, int, const void*, char, unsigned long long, unsigned int)
DownloadLib::20GetDownloadRangeInfo(unsigned long long, int, const char*, unsigned int)
DownloadLib::20GetUploadListenPorts(void*, t, ?, 0, _, ?, 0, _, ?, 0, _)
DownloadLib::20NotifyNetWorkCarrier(1, 5, _, N, e, t, W, o, r, k, C, a, r, r, int, e, r)
DownloadLib::20SetCandidateResSpeed(unsigned long long, int)
DownloadLib::21GetUploadFileInfoList(void*, 1, 5, _, U, p, l, o, a, double, F, int, l, e, I, n, float, o, unsigned int*)
```

共 101 个不重复的 DownloadLib 函数签名。

---

## 未完成（后续阶段）

1. **`TAG_XL_TASK_INFO_EX` 布局**：GetTaskInfo 的输出结构体（字段 offset）— 任务状态/进度/速度的查询接口
   - 当前阻塞：内部填充函数通过虚函数表多级调度，静态分析难以追踪
   - 替代方案：写 macOS 测试程序，调用 API 后 hex dump 内存

2. **`XLInitParam` 各字段语义**：9 个 (ptr, len) 对的具体含义（路径、URL、回调？）

3. **完整 C 导出列表**：153 个 _XL 函数地址映射
   - 当前有 ~30 个关键函数地址
   - 需要完整的导出表解析或系统化字符串搜索

4. **字符串编码确认**：是 UTF-8 窄字符串（`char*` + `u32 len`），还是另有 UTF-16

5. **`TAG_TASK_PARAM_BT` 完整布局**：+0x00/+0x04/+0x08 三个 u32 的语义

---

## 工具链

- Python 3.13 + capstone 5.0.7（ARM64 模式）
- macholib 1.16.4（Mach-O 解析）
- 提取脚本：`scripts/research/xunlei/macho_*.py`、`dmg_*.py`、`elf_*.py`、`apk_*.py`
- 二进制：`extracted_macos/.../DownloadKit_arm64.bin`（已提取 arm64 slice）
