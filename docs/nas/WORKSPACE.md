# 工作区持久化规约（WORKSPACE PROTOCOL）

> 背景：2026-08-31 沙盒重建事故——本地工作区（worklog / repo clone /
> `~/.nas-engine-test/` 热态引擎工作区）全部丢失，仅 GitHub 远端幸存。
> 本规约确保**任何时刻沙盒蒸发，知识零损失**，损失仅限可重建的执行环境。

## 分层原则

| 层 | 内容 | 生命周期 | 去向 |
|----|------|----------|------|
| L1 知识层 | 校准结论文档（A*/A6_PREP*/ASSET_MANIFEST） | 永久 | **git commit（每次会话必推）** |
| L2 代码层 | `scripts/nas/*.py` 校准器/探针/ops | 永久 | **git commit** |
| L3 证据层 | 原始回执 JSON / 引擎日志摘录 / 前端 bundle（**脱敏 token**） | 永久 | **git commit** |
| L4 资产层 | 官方 SPK 等大二进制 | 版本级 | **git commit**（<100MB 直存；>100MB 走 Release） |
| L5 易失层 | `~/.nas-engine-test/` 下 KV 凭据（*.bolt）/ 运行日志 / 下载物 | 会话级 | **绝不入仓**，`a6_ops.py extract` + A2 登录链可重建 |

## 会话收尾五步（不可省略）

```bash
# 1. 快照核对（该提交什么/绝不能提交什么）
python3 scripts/nas/a6_ops.py snapshot

# 2. 新证据落位 docs/nas/evidence/<阶段>/（JSON 先脱敏 pan-auth/token/device_id）
# 3. worklog.md 追加 Task 段（根目录 /home/z/my-project/worklog.md，非仓内）
# 4. 提交
git add -A && git commit -m "..." 
# 5. 推送 + PR body 追加（python3 scripts/nas/pr_update.py，token 失效则明示用户）
git push origin feat/nas-remote-identity
```

## 冷恢复（沙盒重建后从零到可执行）

```bash
git clone -b feat/nas-remote-identity https://github.com/mmqz/smart-downloader.git
cd smart-downloader
python3 scripts/nas/a6_ops.py extract     # 引擎二进制落位 + SHA256 校验
python3 scripts/nas/a6_ops.py envconfig   # 群晖配方
python3 scripts/nas/a2_device_flow.py     # 凭据注入（交互: 设备码/扫码确认）
python3 scripts/nas/a6_probe.py full      # A6 实弹
```

## 凭据与脱敏红线

- KV bolt 库、pan-auth JWT、device_id#hex32 完整串、cookie：**只进 L5**。
- 证据 JSON 入仓前正则清洗：`pan-auth` 值、`token` 字段、`Authorization` 头。
- device_id 展示时保留 `device_id#` + 前 6 hex 即可定位。

## 配额日历（执行类会话开工前先看）

- 任务创建：**每日 3 次，失败也计数，北京时间 0 点重置**（A4/A5 双探针实锤）。
- 超级加速 try_speed：独立配额 total=3，未达 RUNNING 不扣。
- 静态分析/写码/文档/资产类会话：零额度，任何时间可做。
