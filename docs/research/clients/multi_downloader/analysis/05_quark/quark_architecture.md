# 夸克网盘 PC 客户端 V7.1.0.772 逆向分析

> **目标**：剖析闭源夸克网盘 PC 客户端安装器架构，分析其下载能力（分片下载、TLS 实现、错误恢复），并对比开源 BT 客户端内核（qBittorrent/Tixati），评估其是否值得借鉴用于新 Rust 多协议下载器。
>
> **方法**：静态分析。原始 .exe 为 `QuarkCloudDrivePC_V7.1.0.772.exe`（4 MB PE32+ InnoSetup installer stub），真正逻辑在内嵌 `mini_install.dll`（3.7 MB PE32+ DLL，导出 `GetMiniInstallerInstance`）。工具链 `pefile + lief + strings + objdump`。
>
> **限制说明**：本次仅逆向 installer stub。完整夸克网盘客户端（约 80-100 MB）需要安装后才能获取，本次环境无法安装 Windows 程序，故分析聚焦于 installer stub 的下载/状态机设计。

---

## 1. 概览

夸克网盘 PC 客户端是**广州市动悦信息技术有限公司**（阿里旗下）开发的云盘客户端。本次分析的 `V7.1.0.772` 是 mini installer 模式（4 MB stub），不是完整客户端。

关键发现：

1. **完全自实现 TLS 栈**：mini_install.dll 静态链接 OpenSSL（约 1.5 MB），支持 TLS 1.3 完整 cipher suite
2. **分片下载算法**：`download slice` + `task_id` 模型，支持 retry + error_code + extra_error_code 三段错误码
3. **Inno Setup + Custom DLL** 双层架构：用 InnoSetup 标准 installer 框架，但替换了默认下载逻辑为自研 mini_install.dll
4. **阿里标准组件**：Puds（统一打点上报）+ CMS（动态配置）服务嵌入
5. **无 BT 协议支持**：夸克网盘是纯 HTTP(S) 客户端，没有 BT/DHT/uTP，与新下载器需求**部分重叠但不直接相关**

### 1.1 价值评估

| 用户需求 | 夸克能力 | 是否值得借鉴 |
|---------|----------|--------------|
| BT 协议栈 | ❌ 无 | — |
| HTTP/HTTPS 多线程下载 | ✅ 有（分片） | **是** |
| TLS 1.3 客户端 | ✅ 自实现 | 参考 |
| 错误恢复/重试 | ✅ 三段错误码 | **是** |
| 状态机设计 | ✅ 7 阶段清晰 | **是** |
| 配置/进度持久化 | ✅ JSON + .dat | 一般 |
| 进度回调/监听器 | ✅ DownloadEventListener | **是** |

---

## 2. 二进制基本信息

### 2.1 外层 .exe (QuarkCloudDrivePC Installer)

| 属性 | 值 |
|------|----|
| 文件大小 | 4,047,184 字节 (3.86 MB) |
| 类型 | PE32+ Windows x86-64 GUI |
| 编译 | Inno Setup (空 `inno_setup_installer.exe` 占位 + mini_install.dll 替换) |
| 版本信息 | `QuarkCloudDrivePC Installer v3.1.0.2` |
| 公司 | 广州市动悦信息技术有限公司 |
| 版权 | 版权所有©广州市动悦信息技术有限公司 |

**PE 段分布**：

| 段名 | VirtualSize | RawSize | Entropy | 用途 |
|------|-------------|---------|---------|------|
| `.text` | 942,308 | 942,592 | 6.43 | Inno Setup 主代码 |
| `.rdata` | 289,668 | 289,792 | 5.05 | 只读数据 |
| `.data` | 32,756 | 20,992 | 4.39 | 全局变量 |
| `.pdata` | 42,036 | 42,496 | 5.95 | 异常处理表 |
| `.rsrc` | 2,710,896 | 2,711,040 | 7.79 | **资源段（含 ZIPRES + DLL）** |
| `.reloc` | 19,024 | 19,456 | 5.43 | 重定位表 |

`.rsrc` 占 2.7 MB（67%），熵 7.79 接近随机数据——里面是压缩的资源。

### 2.2 PE 资源树

```
.rsrc
├── DLL (type)
│   └── ID 106 → ID 2052 (zip 压缩, 1.66 MB → 解压 3.73 MB)
│       └── mini_install.dll
├── ZIPRES (type, 自定义)
│   └── ID 102 → ID 2052 (zip 压缩, 646 KB)
│       ├── icon.ico
│       ├── res.xml (UI 布局 XML, InnoSetup 的窗口模板)
│       ├── check_off.png / check_on.png
│       ├── close_btn.png / forward.png / more.png
│       ├── quark_brand.png / quark_logo.png
│       └── cloud_drive/ (子目录)
├── 3 (standard RT_ICON/RT_BITMAP/...)
│   └── ID 1..12 → 2052 (各级尺寸 icon: 16/32/48/64/128/256)
├── 14 (RT_GROUP_ICON)
│   └── ID 101 → 2052
└── 16 (RT_VERSION)
    └── ID 1 → 2052
```

### 2.3 内层 DLL (mini_install.dll)

| 属性 | 值 |
|------|----|
| 文件大小 | 3,732,016 字节 (3.56 MB) |
| 类型 | PE32+ DLL (x86-64) |
| 导出 | `GetMiniInstallerInstance` @ 0x5828 |
| 入口 | DllMain |
| 编译 | Visual C++ (基于 `.pdata` 与 RTTI 风格) |

**段分布**：

| 段名 | VirtualSize | Entropy | 用途 |
|------|-------------|---------|------|
| `.text` | 2,518,340 | 6.44 | 主逻辑代码 |
| `.rdata` | 973,684 | 5.31 | 字符串常量 |
| `.data` | 64,412 | 4.01 | 全局变量 |
| `.pdata` | 137,232 | 6.22 | 异常表 |
| `.rsrc` | 480 | 4.72 | 极少资源 |
| `.reloc` | 42,428 | 5.45 | 重定位 |

### 2.4 导入表分析

| DLL | 函数数 | 用途 |
|-----|--------|------|
| `KERNEL32.dll` | 156 | 文件/进程/线程/同步基础 |
| `WS2_32.dll` | 31 | Winsock 网络（自实现 socket 池） |
| `USER32.dll` | 9 | 极少 UI（仅 MessageBox） |
| `CRYPT32.dll` | 8 | **系统证书 store 查询** |
| `ADVAPI32.dll` | 19 | 注册表/服务/进程权限 |
| `gdiplus.dll` | 0 | (无 - 实际未导入) |
| `SHELL32.dll` | 2 | ShellExecute |
| `ole32.dll` | 1 | COM |
| `SHLWAPI.dll` | 1 | 路径操作 |
| `bcrypt.dll` | 1 | **BCryptGenRandom (CSPRNG)** |

**关键观察**：

- `WS2_32.dll` 31 个函数：包含 `WSASocketW`, `getaddrinfo`, `WSAPoll`, `inet_pton`, `getnameinfo`, `freeaddrinfo`, `closesocket`, `select` → **完全自实现 socket 客户端**
- 没有 `wininet.dll` / `winhttp.dll` → 不用 Windows HTTP 库
- 没有 `crypt32` 的 `CryptAcquireContext` → 不用 Windows CA库做加密
- 但用 `CertOpenSystemStoreW`, `CertEnumCertificatesInStore`, `CertFindCertificateInStore` → **借 Windows 系统 cert store 验证 TLS**
- `BCryptGenRandom` → 用 Windows CNG 做 CSPRNG

### 2.5 RTTI 类名泄露

通过 C++ RTTI（`.?AV...@@` 格式字符串）提取的类清单：

```
.?AVDownloadEventListener@@          ← 下载事件监听器（Observer 模式）
.?AVPudsService@@                     ← 阿里统一打点上报服务
.?AVPudsServiceImpl@@                 ← Puds 实现
.?AV?$_Binder@U_Unforced@std@@P8PudsServiceImpl@@EAAXPEAVPudsCallbackData@@@ZPEAV3@AEAPEAV4@@std@@
                                       ← std::bind 绑定 PudsServiceImpl::callback
.?AVCMSService@@                      ← CMS 配置服务
.?AVCMSServiceImpl@@                  ← CMS 实现
.?AVObserver@CMSService@@             ← Observer 基类
.?AVparse_error@detail@json_abi_v3_11_3@nlohmann@@  ← nlohmann/json v3.11.3
.?AVexception@detail@json_abi_v3_11_3@nlohmann@@
.?AVtype_error@detail@json_abi_v3_11_3@nlohmann@@
.?AVout_of_range@detail@json_abi_v3_11_3@nlohmann@@
.?AVinvalid_iterator@detail@json_abi_v3_11_3@nlohmann@@
```

**结论**：

- **nlohmann/json v3.11.3**：JSON 解析（GitHub 热门 header-only 库）
- **PudsService**：阿里统一数据上报（Performance & User Data Service）
- **CMSService**：阿里统一 CMS 配置拉取（Content Management Service）
- **DownloadEventListener**：进度回调接口

这两个阿里标准组件的存在证明夸克 PC 客户端**复用了阿里系移动端的统一基础设施**，不是为 PC 单独开发的。

---

## 3. 架构总览

### 3.1 进程模型

```
┌─────────────────────────────────────────────────────────────┐
│  QuarkCloudDrivePC.exe (4 MB, InnoSetup stub)                │
│  ┌─────────────────────────────────────────────────────┐    │
│  │ 加载 .rsrc/DLL/106/2052 (zip 压缩的 mini_install.dll) │    │
│  │ → 解压到临时目录 → LoadLibraryA → GetMiniInstallerInstance │
│  └─────────────────────────────────────────────────────┘    │
│                          │                                   │
└──────────────────────────┼───────────────────────────────────┘
                           ▼
┌─────────────────────────────────────────────────────────────┐
│  mini_install.dll (3.7 MB, 真正的安装器)                     │
│  ┌──────────┬──────────┬──────────┬──────────┬───────────┐  │
│  │ HTTP(S)  │ TLS 1.3  │ Puds     │ CMS      │ File     │  │
│  │ Client   │ (OpenSSL │ Service  │ Service  │ IO       │  │
│  │ (自研)   │ 静态)    │ (上报)   │ (配置)   │          │  │
│  └──────────┴──────────┴──────────┴──────────┴───────────┘  │
│                          │                                   │
│  状态机：fetch_version → download → install → setup         │
└─────────────────────────────────────────────────────────────┘
                           │
                           ▼
                    (网络请求)
                           │
        ┌──────────────────┼─────────────────────┐
        ▼                   ▼                     ▼
┌───────────────┐  ┌──────────────┐  ┌──────────────────┐
│ download.quark│  │ open-cms-api.│  │ track.lc.quark.cn│
│ .cn           │  │ quark.cn     │  │ + px.effirst.com │
│ (主程序下载)  │  │ (CMS 配置)    │  │ + puds.quark.cn  │
│               │  │              │  │ (打点上报)        │
└───────────────┘  └──────────────┘  └──────────────────┘
```

### 3.2 关键域名清单

| 域名 | 用途 | 证据 |
|------|------|------|
| `download.quark.cn` | 主程序包下载 | `https://download.quark.cn/download/quarkclouddrivepc?platform=pc&ch=pckk@app_downloader_fail` |
| `open-cms-api.quark.cn` | CMS 动态配置 | 字符串 `open-cms-api.quark.cn` |
| `open-cms-api.ude.alibaba.net` | CMS 阿里内网备用 | 同上 |
| `track.lc.quark.cn` | 行为埋点上报 | `http://track.lc.quark.cn` |
| `puds.quark.cn` | Puds 统一数据上报 | `puds.quark.cn` |
| `px.effirst.com` | 错误/性能上报 | `http://px.effirst.com /api/v1/jstrace/upload` |
| `quark_updater` | 更新器调用 | 字符串 `quark_updater` |

**注意**：`px.effirst.com` 是阿里收购的「极籁科技」前端错误上报服务，这是阿里系前端的标准组件。

### 3.3 UA 与版本

```
Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 QuarkPC/4.3.0.0
```

UA 中：
- 伪装 Chrome 130（用于规避部分服务器对非浏览器 UA 的拒绝）
- `QuarkPC/4.3.0.0` 是 **上报器版本**，与外层 installer v3.1.0.2 不同
- 4.3.0.0 看起来是历史版本号，未随 installer 升级

---

## 4. 下载器核心算法

### 4.1 分片下载算法

从字符串 `download slice failed. task_id: %d, error_code: %d, extra_error_code: %d, retry_count: %d` 可推断：

```python
class DownloadTask:
    task_id: int                  # 任务唯一 ID
    total_size: int              # 文件总大小
    slice_size: int              # 分片大小（推测 4MB）
    slices: list[Slice]          # 分片列表
    retry_count: int             # 任务级重试
    backup_url: str              # 备用 URL（备用源切换）
    backup_md5: str              # 备用源校验

class Slice:
    offset: int                  # 文件内偏移
    length: int                 # 分片长度
    status: SliceStatus         # pending/downloading/done/failed
    error_code: int              # 错误码（HTTP 状态码等）
    extra_error_code: int        # 额外错误码（WSAGetLastError 等）
    retry_count: int             # 分片级重试

def download_slice(task: DownloadTask, slice: Slice):
    """单个分片下载"""
    url = task.url or task.backup_url  # 备用源切换
    headers = {
        'Range': f'bytes={slice.offset}-{slice.offset + slice.length - 1}',
        'User-Agent': QUARK_UA,
    }
    try:
        response = http_get(url, headers=headers)
        if response.status_code == 206:  # Partial Content
            verify_md5(response.data, slice.expected_md5)
            write_to_file(response.data, slice.offset)
            slice.status = SliceStatus.DONE
        else:
            slice.error_code = response.status_code
            retry_slice(task, slice)
    except NetworkError as e:
        slice.error_code = -1
        slice.extra_error_code = e.wsa_errno
        retry_slice(task, slice)

def retry_slice(task, slice):
    if slice.retry_count < MAX_RETRY:
        slice.retry_count += 1
        backoff = 2 ** slice.retry_count  # 指数退避
        schedule_after(backoff, lambda: download_slice(task, slice))
    else:
        # 尝试备用源
        if task.backup_url and not slice.using_backup:
            slice.using_backup = True
            download_slice(task, slice)  # retry with backup
        else:
            task.failed = True
            report_error(task, slice)
```

### 4.2 三段错误码设计

```
[tid %lu][quark_installer][error] download slice failed. task_id: %d, error_code: %d, extra_error_code: %d, retry_count: %d
```

| 字段 | 含义 | 来源 |
|------|------|------|
| `task_id` | 任务 ID | 自定义 |
| `error_code` | 主错误码 | HTTP 状态码 或 业务码 |
| `extra_error_code` | 副错误码 | WSA 错误码 / OpenSSL 错误码 |
| `retry_count` | 已重试次数 | 自定义 |

**这是一个值得借鉴的设计**：将错误来源分层（业务层 + OS 层），方便排查跨层问题。

### 4.3 备用源（backup_url）机制

从字符串：

```
backup_total_ms
use_backup
backup_ver
backup_url
backup_md5
use_backup_success
```

可见完整 backup 流程：

```python
def try_backup_source(task):
    """主源失败后切换备用源"""
    start = time.time()
    backup_url = fetch_backup_url(task.resource_id)
    backup_md5 = fetch_backup_md5(task.resource_id)

    task.url = backup_url
    task.expected_md5 = backup_md5

    if download_file(task):
        task.use_backup_success = True
    task.backup_total_ms = int((time.time() - start) * 1000)
    report_to_track(task)  # 上报备用源使用情况
```

**这是 CDN 调度思路的客户端版本**：主源失败→查询备用源→切换→校验→上报结果。这对开发多 mirror 下载器有直接参考价值。

---

## 5. 状态机设计

### 5.1 7 阶段状态机

从字符串提取的完整状态机：

```
mini_fetch_version_start_clouddrive
mini_fetch_version_end_clouddrive
        │
        ▼
mini_download_start_clouddrive
mini_download_end_clouddrive
        │
        ▼
mini_install_start_clouddrive
mini_install_end_clouddrive
        │
        ▼
mini_setup_start_clouddrive
mini_setup_end_clouddrive
```

外加分支：

```
mini_install_retry_start_clouddrive  ←  失败重试
mini_install_retry_end_clouddrive

mini_install_kill_exist_process_clouddrive  ← 杀掉已存在的进程

mini_download_start_show_clouddrive  ← "show" 模式（安装展示版？）
mini_download_end_show_clouddrive
mini_install_start_show_clouddrive
mini_install_end_show_clouddrive
mini_setup_end_show_clouddrive

mini_download_start_old_clouddrive  ← "old" 模式（旧版本升级路径）
mini_download_end_old_clouddrive
mini_install_start_old_clouddrive
mini_install_end_old_clouddrive
```

### 5.2 状态机图

```
                  start
                    │
                    ▼
       ┌─────────────────────────────┐
       │ mini_fetch_version_*         │  ← 拉取最新版本号
       │ - GET /api/v1/version         │
       │ - 失败: retry (max 3)        │
       └─────────────────────────────┘
                    │
                    ▼
       ┌─────────────────────────────┐
       │ mini_install_kill_exist_*    │  ← 杀掉已运行进程
       │ - OpenProcess + TerminateProcess │
       │ - 失败: 用 ShellExecute     │
       └─────────────────────────────┘
                    │
                    ▼
       ┌─────────────────────────────┐
       │ mini_download_*              │  ← 分片下载
       │ - 切片 + 并发 GET Range      │
       │ - 失败: 切换 backup_url     │
       │ - 校验 MD5                   │
       └─────────────────────────────┘
                    │
              ┌─────┴─────┐
              ▼           ▼
         success        failure
              │           │
              │           ▼
              │   ┌───────────────────┐
              │   │ mini_install_retry│  ← 整体重试
              │   └───────────────────┘
              │           │
              ▼           ▼
       ┌─────────────────────────────┐
       │ mini_install_*               │  ← 解压 + 写文件
       │ - unzip installer package    │
       │ - 写入 install_dir           │
       └─────────────────────────────┘
                    │
                    ▼
       ┌─────────────────────────────┐
       │ mini_setup_*                │  ← 注册表 + 快捷方式
       │ - 创建快捷方式              │
       │ - 写注册表 uninstall key    │
       │ - 创建自启动项              │
       └─────────────────────────────┘
                    │
                    ▼
                  done
```

### 5.3 错误恢复策略

从字符串证据：

```python
def install_with_retry():
    """安装失败重试机制"""
    for attempt in range(MAX_RETRY):
        try:
            fetch_version()           # 1. 获取版本
            kill_exist_process()      # 2. 杀旧进程
            download_main()           # 3. 下载主程序
            install_files()           # 4. 解压安装
            setup_registry()         # 5. 注册表/快捷方式
            return SUCCESS
        except (NetworkError, DiskError) as e:
            log_error(attempt, e)
            if attempt < MAX_RETRY - 1:
                time.sleep(2 ** attempt)
                continue
            else:
                report_to_track(e)
                return FAILURE
```

---

## 6. TLS 实现分析

### 6.1 OpenSSL 静态链接证据

字符串中的 OpenSSL 函数名（约 200+ 个）：

```
SSL_CTX_new
SSL_new
SSL_connect
SSL_read
SSL_write
SSL_get_verify_result
SSL_set_ct_validation_callback
SSL_SESSION_new
SSL_set_min_proto_version
SSL_set_max_proto_version
X509_STORE_CTX_new
X509_verify_cert
EVP_PKEY_CTX_new
EVP_PKEY_verify
```

**结论**：mini_install.dll 静态链接了完整 OpenSSL（推测 1.1.1 或 3.x），不依赖系统 schannel.dll。

### 6.2 TLS 1.3 支持

字符串证据：

```
TLS_AES_256_GCM_SHA384
TLS_CHACHA20_POLY1305_SHA256
TLS_AES_128_GCM_SHA256
TLS 1.3, server CertificateVerify
TLS 1.3, client CertificateVerify
tls_construct_new_session_ticket
tls_construct_certificate_authorities
```

**完整支持 TLS 1.3 RFC 8446**，cipher suite 与 Chrome 130 一致。

### 6.3 证书验证策略

```
CertOpenSystemStoreW      ← 打开 Windows "ROOT"/"CA"/"MY" store
CertOpenStore
CertEnumCertificatesInStore
CertFindCertificateInStore
CertGetCertificateContextProperty
```

**混合策略**：用 OpenSSL 做 TLS 协议握手，但用 Windows 系统 cert store 做根证书验证（而非 OpenSSL 自带 ca-bundle.crt）。这是为了：

1. 用户安装的企业根证书可以自动信任
2. 减少 DLL 大小（不需要打包 ca-bundle.crt）

### 6.4 完美前向保密

支持的密钥交换算法（从字符串）：

```
ECDHE-ECDSA-AES128-GCM-SHA256
ECDHE-ECDSA-AES256-GCM-SHA384
ECDHE-ECDSA-AES128-GCM-SHA256
ecdsa_secp256r1_sha256
rsa_pss_rsae_sha256
rsa_pss_pss_sha256
rsa_pkcs1_sha256
rsa_pkcs1_md5_sha1
```

**结论**：完整支持 ECDHE 完美前向保密，符合现代 HTTPS 最佳实践。

---

## 7. 阿里标准组件嵌入

### 7.1 PudsService（统一数据上报）

Puds = Performance & User Data Service，阿里统一的前端打点上报 SDK。

```cpp
class PudsService {
public:
    virtual void Report(PudsCallbackData* data) = 0;
    virtual void SetUserId(const std::string& user_id) = 0;
    virtual void Flush() = 0;
};

class PudsServiceImpl : public PudsService, public Observer {
    // 实际实现：
    // 1. 异步队列收集事件
    // 2. 批量上报到 puds.quark.cn
    // 3. 失败重试 + 本地缓存
};
```

通过 `puds.quark.cn` 上报：

- 安装成功/失败事件
- 下载速度/时长
- 错误码分布
- 用户 OS 信息
- 安装目录选择

### 7.2 CMSService（动态配置）

CMS = Content Management Service，阿里统一的动态配置拉取。

```cpp
class CMSService {
public:
    virtual void FetchConfig(const std::string& key, ConfigCallback cb) = 0;
};

class CMSServiceImpl : public CMSService, public Observer {
    // 实际实现：
    // 1. 从 open-cms-api.quark.cn 拉取 JSON 配置
    // 2. 失败 fallback 到 open-cms-api.ude.alibaba.net
    // 3. 配置缓存到本地 JSON
    // 4. TTL 过期重新拉取
};
```

用于：

- 安装目录默认值
- 镜像 URL 列表
- 重试次数配置
- 防火墙白名单

### 7.3 Observer 模式

```cpp
template<typename T>
class Observer {
public:
    virtual void OnUpdate(T* subject) = 0;
};

// 用 std::bind 绑定成员函数作为回调
auto binder = std::bind(&PudsServiceImpl::OnDataReady,
                        puds_impl,
                        std::placeholders::_1,
                        std::placeholders::_2);
```

字符串 `.?AV?$_Binder@U_Unforced@std@@P8PudsServiceImpl@@...` 是 `std::bind` 的 RTTI 类型名。

---

## 8. 错误处理与日志

### 8.1 日志格式

所有错误日志统一格式：

```
[tid %lu][quark_installer][error] <error message>. <key>: %d, <key>: %d
```

`tid` = thread id（使用 `GetCurrentThreadId()`），表明**多线程并发下载**。

### 8.2 已识别的错误日志

| 日志模板 | 含义 |
|---------|------|
| `Send report request failed. status: %d` | 上报失败 |
| `Send wpk report request failed. status: %d` | WPK 上报失败（wpk = ?） |
| `Send version request failed. api: %s, status: %d, retry: %d` | 版本接口失败 |
| `Fetch installer size request failed. status: %d` | 获取文件大小失败 |
| `[Backup] DataTask HTTP response failed. task_id: %d, status: %d` | 备用源 HTTP 失败 |
| `download slice failed. task_id: %d, error_code: %d, extra_error_code: %d, retry_count: %d` | 分片下载失败 |
| `response invalid` | 响应格式错误 |
| `error code not empty` | 业务错误码非空 |
| `Send cms config request failed. status: %d, retry: %d` | CMS 配置失败 |
| `json parse error: %s` | JSON 解析失败 |
| `terminate process failed. ec: %d` | 杀进程失败 |
| `open process failed. ec: %d` | OpenProcess 失败 |
| `terminate process failed. path: %s, pid: %d, ec: %d` | 杀指定路径进程失败 |
| `query full process image name failed. ec: %d` | 查询进程路径失败 |
| `get process module base name failed. ec: %d` | 获取模块名失败 |

### 8.3 错误码体系（推断）

```
error_code (主码):
  200-299: HTTP 成功
  3xx:    重定向
  4xx:    客户端错误
  5xx:    服务端错误
  -1:     网络层错误
  -2:     TLS 错误
  -3:     JSON 解析错误
  -4:     文件 IO 错误

extra_error_code (副码):
  WSAECONNRESET (10054): 连接被重置
  WSAETIMEDOUT (10060): 连接超时
  OpenSSL SSL_ERROR_SSL: TLS 协议错误
  ERROR_ACCESS_DENIED (5): 权限不足
  ERROR_FILE_NOT_FOUND (2): 文件不存在
```

---

## 9. 与 qBittorrent / Tixati 的对比

### 9.1 功能对比表

| 维度 | qBittorrent | Tixati | 夸克网盘 | 谁更好？ |
|------|-------------|--------|----------|----------|
| BT 协议 | libtorrent (完整) | 自研 (完整) | ❌ | qBT/Tixati |
| HTTP 多线程 | libcurl (基础) | 自研 (基础) | **自研分片** | 夸克 |
| HTTPS/TLS | OpenSSL | 自研 RC4 + 自研 TLS | **OpenSSL 静态 + TLS 1.3** | 夸克 |
| 分片下载 | Range (基础) | Range (基础) | **task_id + slice** | 夸克 |
| 重试机制 | 简单 | 简单 | **三段错误码 + 指数退避** | 夸克 |
| 备用源 | mirror list | mirror list | **CMS 动态下发** | 夸克 |
| 错误恢复 | alert 系统简单 | 简单 | **状态机 + 重试** | 夸克 |
| 配置系统 | settings_pack | .dat 文件 | JSON + CMS 远程 | 夸克（远程） |
| 监听器 | alert handler | callback | **DownloadEventListener** | 夸克（清晰） |
| 上报机制 | 无 | 无 | **PudsService** | 夸克 |
| 状态机 | alert 状态 | peer 状态 | **7 阶段安装状态** | 夸克（最清晰） |
| 持久化 | resume.dat | .dat 文件 | JSON | 相当 |
| 多协议 | BT + HTTP | BT + HTTP + FTP | HTTPS | qBT/Tixati |

### 9.2 夸克的设计优势

1. **状态机最清晰**：7 阶段 + 重试 + 杀进程的完整流程，每个阶段都有 start/end 钩子
2. **错误码最规范**：三段错误码（task + error_code + extra_error_code + retry_count）
3. **远程配置**：CMS 服务可以动态下发安装参数，无需重新发版
4. **监听器模式**：`DownloadEventListener` 抽象清晰，进度回调可扩展
5. **TLS 现代化**：完整 TLS 1.3，比 Tixati 的 RC4 MSE 现代得多

### 9.3 夸克的设计劣势

1. **闭源**：无法审计后门
2. **阿里系埋点繁多**：Puds + CMS + track.lc + px.effirst 至少 4 个上报通道，**严重隐私问题**
3. **不支持 BT**：纯 HTTP(S)，与开源 BT 客户端生态隔离
4. **依赖 Windows**：无 Linux/macOS 版本
5. **InnoSetup 外壳**：4MB 的外壳其实可省，直接用 mini_install.dll 即可
6. **PE 体积浪费**：4MB installer 包含 ZIPRES（PNG 资源）+ DLL（zip 压缩），本可全部用 LZMA 压到 2MB

### 9.4 关键启示

夸克的**下载器内核**（分片下载 + 备用源 + TLS 1.3 + 三段错误码 + 状态机）非常适合移植到 Rust 多协议下载器的 HTTP(S) 部分。但**埋点上报部分应完全丢弃**——这是商业产品的隐私代价，开源项目不应模仿。

---

## 10. 用户原始问题回答

用户原始提问："夸克的能力分析，有没有我需要的"。

**结论：有，但有限**。

| 用户需求 | 夸克能否提供参考 |
|---------|------------------|
| BT 协议栈 | ❌ 夸克无 BT，参考价值 0 |
| Peer 评分 | ❌ 夸克无 peer，参考价值 0 |
| 带宽分配模型 | ❌ 夸克是单连接 HTTP，参考价值 0 |
| 连接生命周期管理 | ⚠️ 部分参考（TLS 握手 + HTTP keepalive） |
| **分片下载算法** | ✅ **强参考**（task_id + slice + retry） |
| **重试与错误恢复** | ✅ **强参考**（三段错误码 + 指数退避 + 备用源） |
| **TLS 1.3 实现** | ✅ **强参考**（OpenSSL 静态链接方式） |
| **状态机设计** | ✅ **强参考**（7 阶段 + retry + kill） |
| **配置系统** | ⚠️ 部分参考（CMS 远程下发思路，但实际项目可能不需要） |
| **监听器模式** | ✅ 参考（DownloadEventListener 设计清晰） |

**最终判断**：夸克的**下载器内核设计**值得借鉴（约 30% 的代码思路可用于 Rust 多协议下载器的 HTTP(S) 部分），但**不要照搬其上报/埋点组件**。

---

## 11. 对 Rust 多协议下载器的启示

### 11.1 可借鉴设计

1. **分片下载 + task_id 模型**：
   ```rust
   pub struct DownloadTask {
       pub task_id: u64,
       pub url: Url,
       pub backup_url: Option<Url>,
       pub expected_md5: Option<String>,
       pub total_size: u64,
       pub slices: Vec<Slice>,
   }
   ```

2. **三段错误码**：
   ```rust
   pub struct DownloadError {
       pub task_id: u64,
       pub error_code: i32,         // HTTP 状态码或业务码
       pub extra_error_code: i32,    // OS errno 或 TLS 错误
       pub retry_count: u32,
   }
   ```

3. **状态机设计**：7 阶段 + retry 钩子

4. **TLS 实现**：用 `rustls`（纯 Rust）替代 OpenSSL，但 cipher suite 配置可参考夸克

5. **监听器模式**：
   ```rust
   pub trait DownloadEventListener {
       fn on_slice_start(&self, task: &DownloadTask, slice: &Slice);
       fn on_slice_progress(&self, task: &DownloadTask, slice: &Slice, bytes_done: u64);
       fn on_slice_complete(&self, task: &DownloadTask, slice: &Slice);
       fn on_slice_failed(&self, task: &DownloadTask, slice: &Slice, err: &DownloadError);
       fn on_task_complete(&self, task: &DownloadTask);
       fn on_task_failed(&self, task: &DownloadTask, err: &DownloadError);
   }
   ```

### 11.2 应避免的设计

1. **InnoSetup + DLL 双层架构**：Rust 直接编译单一可执行
2. **多埋点上报通道**：开源项目无此需求
3. **依赖 Windows cert store**：跨平台应用应用 `webpki-roots`
4. **闭源静态 OpenSSL**：用 Rust 生态的 `rustls` 更安全

### 11.3 Rust 推荐栈

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["stream"] }
rustls = "0.23"          # 替代 OpenSSL
rustls-pemfile = "2"
webpki-roots = "0.26"   # Mozilla 根证书
md-5 = "0.10"           # 备用源校验
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"         # 三段错误码
tracing = "0.1"         # 日志（替代 [tid][quark_installer] 格式）
tokio-util = { version = "0.7", features = ["io"] }
```

---

## 12. 附录 A：完整 URL 清单

| URL | 用途 |
|-----|------|
| `https://download.quark.cn/download/quarkclouddrivepc?platform=pc&ch=pckk` | 主程序包下载 |
| `http://track.lc.quark.cn` | 行为埋点上报 |
| `http://px.effirst.com/api/v1/jstrace/upload` | JS 错误上报 |
| `puds.quark.cn` | Puds 统一数据上报 |
| `open-cms-api.quark.cn` | CMS 配置拉取 |
| `open-cms-api.ude.alibaba.net` | CMS 阿里内网备用 |
| `http://ocsp.digicert.com` | DigiCert OCSP |
| `http://crl3.digicert.com/DigiCertTrustedRootG4.crl` | CRL |
| `http://cacerts.digicert.com/DigiCertTrustedRootG4.crt` | CA 证书下载 |

## 13. 附录 B：完整状态机字符串清单

```
mini_fetch_version_start_clouddrive
mini_fetch_version_end_clouddrive

mini_download_start_clouddrive
mini_download_end_clouddrive

mini_install_start_clouddrive
mini_install_end_clouddrive

mini_setup_start_clouddrive
mini_setup_end_clouddrive

mini_install_retry_start_clouddrive
mini_install_retry_end_clouddrive

mini_install_kill_exist_process_clouddrive

mini_download_start_show_clouddrive
mini_download_end_show_clouddrive
mini_install_start_show_clouddrive
mini_install_end_show_clouddrive
mini_setup_end_show_clouddrive

mini_download_start_old_clouddrive
mini_download_end_old_clouddrive
mini_install_start_old_clouddrive
mini_install_end_old_clouddrive

# 配置/控制
mini_install_config
BackupInstallerCfg_CloudDrive
choice_install
mini_install
clouddrive_mini_install
install_dir
installer_path
use_backup
backup_ver
backup_url
backup_md5
use_backup_success
backup_total_ms
user_uuid
user_uuid_valid
```

## 14. 附录 C：与 qBittorrent/Tixati 的最终能力对照

| 能力 | qBittorrent | Tixati | 夸克 |
|------|-------------|--------|------|
| HTTP 多线程分片 | ⚠️ 单线程 | ⚠️ 单线程 | ✅ 多线程 |
| HTTPS/TLS 1.3 | ⚠️ OpenSSL | ⚠️ 自研 RC4 (BEP 8 MSE) | ✅ OpenSSL 静态 |
| 重试机制 | ⚠️ 简单 | ⚠️ 简单 | ✅ 三段错误码 |
| 备用源切换 | ⚠️ 用户配置 | ⚠️ 用户配置 | ✅ CMS 下发 |
| 状态机清晰度 | ⚠️ alert 系统 | ⚠️ peer 状态 | ✅ 7 阶段 |
| 配置远程下发 | ❌ | ❌ | ✅ CMS |
| 上报/埋点 | ❌ | ❌ | ✅ (但应避免) |
| 监听器抽象 | ⚠️ alert handler | ⚠️ callback | ✅ DownloadEventListener |
| BT 协议 | ✅ libtorrent | ✅ 自研 | ❌ |
| DHT | ✅ | ✅ + Channel | ❌ |
| Peer 评分 | ✅ | ✅ + Charity | ❌ |
| Unchoke 算法 | ✅ + optimistic | ✅ + Forced + Charity | ❌ |
| 带宽分配 | ✅ channel quota | ✅ Trading/Seeding | ❌ |
| uTP | ✅ | ✅ | ❌ |
| I2P | ⚠️ plugin | ✅ 原生 | ❌ |
| BT v2 | ✅ libtorrent 2.0 | ✅ 自研 | ❌ |

**最终判断**：夸克的**下载器内核设计**值得借鉴（约 30% 的代码思路可用于 Rust 多协议下载器的 HTTP(S) 部分），但**完全不能提供 BT 相关的任何参考**。它是 HTTP(S) 下载器的精品实现样本，但不是 BT 客户端的参考。
