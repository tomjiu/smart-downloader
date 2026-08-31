# A2 校准原始证据（Task 31，2026-08-31 云端执行）

> 采集工具：`scripts/nas/a2_*.py`（详见 `docs/nas/A2_CALIBRATION_FINDINGS.md`）
> 敏感处理：token 类字段在采集时即以 `…<redacted>` 截断；device_code/user_code
> 均为 120s 时效一次性码（早已过期），仅存流程形状证据。

| 文件 | 校准轮次 | 结论指向 |
|------|----------|----------|
| `a2_result.json` | R1：预置过期 token + `DriveAuthorizationTokenPath` | 引擎同秒 `DoLoginQrcode`，无 refresh 尝试 → #8 否决 |
| `a2_result_preset2.json` | R2：预置 fresh token（expires_in=7200） | 同秒 `DoLoginQrcode`，与 token 时效无关 → #8 否决强化 |
| `a2_result_final.json` | HostXluser 非白名单值（http://127.0.0.1:8899） | panic 暴露白名单仅 `xluser-ssl` / `dev-xluser-ssl` → §3 注入前提 |
| `ns_login_result.json` | R4：unshare ns + bind-mount 伪 hosts + 443 TLS MITM | `token_200` / KV 三库变更 → `login ok` 全链路 |
| `a2_result_warmboot.json` | 热启动（纯在线零 MITM，launcher 入口） | DriveListen t=0s 就位、runner 注册、#9/#10 路由面实测 |
| `device_flow.json` | RFC 8628 解耦设备码流状态 | expires_in=120 / interval=2 → 服务端解耦可行性参数 |

热态工作区（`~/.nas-engine-test/data/.drive/`，KV 三库 + .backup）含活凭据，
**有意不入库**；沙盒重置后按 FINDINGS §3 注入链重建。
