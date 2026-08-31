# A3 校准原始证据（Task 31，2026-08-31）

| 文件 | 内容 |
|------|------|
| `a3_boot.json` | web UI 抓取轮：GET / 4024B（含 uiauth 注入）、403 错误全量、auth 端点面 404 矩阵 |
| `a3_result_final.json` | 终局轮：pan-auth JWT 解锁后 /drive/v1/tasks 200、try_speed get_info 200、apply 200 全响应 |
| `index.html` | 引擎渲染首页原文——`uiauth()` 注入函数 + UIAuth JWT（HS256，3 天）即为鉴权门钥匙 |
| `assets/` | 前端 bundle（index-1ded6b9a.js 1.5MB 等）——`pan-auth` 头名出处（HTTP 封装 `Zt`）与后续任务创建载荷逆向素材 |

敏感说明：JWT 已于 exp（iat+3d）后自然失效；无账号凭据。
