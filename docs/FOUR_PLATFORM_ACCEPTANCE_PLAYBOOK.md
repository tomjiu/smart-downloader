# 四平台端到端验收剧本（P4-1，2026-08-31）

> 收尾计划 DoD 的验收层：**四平台跑通同一条端到端链**
> 登录 → 提交 → 下载 → 进度 → 完成。
> 判据全部为可观察输出（HTTP 状态/文件落盘/日志行），不接受「应该可以」。

## S0. 通用准备（四平台一致）

```bash
# 构建产物
#   Windows:  cargo build --release -p smart-dl-daemon --features xunlei
#   Linux:    cargo build --release -p smart-dl-daemon --features bt
#   macOS:    cargo build --release -p smart-dl-daemon
#   Android:  scripts/build_android.sh 或 docs/ANDROID_TERMUX_DEPLOY.md 路线 B

# 配置最小化（各平台 dest_root 按需改）
cat > smart-dl.toml <<EOF
[server]
addr = "127.0.0.1:8787"
[download]
dest_root = "./downloads"
[provider]
enabled = true
[provider_xunlei]
enabled = true
tier = "web"
EOF
```

## S1. 单测基线

```bash
cargo test -p smart-dl-core -p smart-dl-provider -p smart-dl-httpdl -p smart-dl-daemon
# 判据：0 failed（当前 Linux x86_64 基线 543 passed，2026-08-31）
```

## S2. 登录（每平台 × 每档）

```bash
./smart-dl-daemon xunlei-login --tier web --qr      # web 档
./smart-dl-daemon xunlei-login --tier nas --qr      # nas 档（可选，多档并存验证）
# 判据：终端打印「✅ 登录成功！user_id: …」
#       web → xunlei_auth.json / nas → xunlei_auth_nas.json（0600）
#       同账号两档先后登录后，第一档 refresh 不失效（互踢检查：
#       等待 >10min 后对第一档 daemon GET /status 无 401）
```

## S3. 提交 → 进度 → 完成（云端离线链）

```bash
# 用一个公共磁力（Ubuntu 22.04 LTS ISO）
./smart-dl-daemon add magnet:?xt=urn:btih:<infohash>&dn=ubuntu-22.04-live-server.iso \
  --dest ./downloads --provider xunlei
./smart-dl-daemon status <task-id>
# 判据：status 经 Queued/Downloading → Ready（云端离线 COMPLETE）
#       建任务响应 params.client_id 与档位一致（web=Xqp0…）——身份观察点
```

## S4. 取直链 → 本地落盘

```bash
# Ready 后 daemon 自动 resolve → httpdl 落盘（cloud fallback 链）
ls -la ./downloads/ubuntu-22.04-live-server.iso
sha256sum ./downloads/ubuntu-22.04-live-server.iso   # 与官方 ISO 校验和比对
# 判据：文件存在、体积与云端一致、SHA256 匹配官方值
```

## S5. 纯 HTTP 直链（不走云盘）

```bash
./smart-dl-daemon add https://releases.ubuntu.com/22.04/ubuntu-22.04-live-server-amd64.iso \
  --dest ./downloads
# 判据：多线程分片推进，httpdl 日志出现 206 分片；断网 10s 重连后续传
```

## S6. daemon 常驻与恢复

```bash
# 杀进程重启
./smart-dl-daemon serve --config smart-dl.toml &
kill %1 && ./smart-dl-daemon serve --config smart-dl.toml
# 判据：tasks.json 恢复任务列表；未完成任务从断点续传（不重头）
```

## S7. 平台判定矩阵（汇总）

| # | 平台 | S1 单测 | S2 登录 | S3 云端链 | S4 落盘 | S5 直链 | S6 常驻 | 判定 |
|---|------|---------|---------|-----------|---------|---------|---------|------|
| 1 | Windows（SDK 引擎 + provider） | ☐ | ☐ | ☐ | ☐ | ☐ | ☐ | 真机项 |
| 2 | Linux x86_64（本仓 CI 环境） | ✅ 543 | ✅（2026-08-25 实测） | ☐ 活体账号 | ☐ | ✅ 单测+冒烟 | ✅ | **已达成**（云端活体 S3/S4 待账号） |
| 3 | macOS（provider + httpdl） | ☐ | ☐ | ☐ | ☐ | ☐ | ☐ | 真机项 |
| 4 | Android/Termux（交叉产物） | N/A（产物直部署） | ✅ 冒烟（nas 档设备码请求 200） | ☐ | ☐ | ☐ | ☐ | **编译判定翻转** ✅，端到端待真机 |

> Linux ✅ 行：S2 登录链为 2026-08-25 端到端实测（研究期）；S3/S4 需活体账号
> 复验一次即闭环。Android 行：P1-3 交叉编译通过 = 判定级缺口清零；S2 已冒烟
> （`--tier nas` 真实设备码 200），S3-S6 待真机。

## 附：失败排查速查

| 症状 | 首查 |
|------|------|
| 登录 4002 captcha_invalid | device_id 档位混用；删 token 重扫 |
| serve 拒绝启动「未知迅雷身份档位」 | `tier` 拼写（web/nas），env SMART_DL_XUNLEI_TIER |
| Android 运行时 linker 报错 | 见 ANDROID_TERMUX_DEPLOY §2 路线 B |
| 云端 403/90120 | 档位是否 nas/docker 档裁剪面；换 web 档复测 |
