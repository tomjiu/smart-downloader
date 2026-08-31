# Task 31 — A3 校准实录：本地 API 鉴权门突破（2026-08-31 云端执行）

> 接续 A2（`A2_CALIBRATION_FINDINGS.md`）。A2 遗留：`/drive/v1/tasks`、
> `try_speed/*` 全部 403（本地鉴权门），指向 KV secret（`GinVerifyToken` 族）。
> **结论先行：鉴权门 = `pan-auth: <JWT>` 请求头；JWT 由引擎自注入 web 首页
> `uiauth()` 函数，一次 `GET /` 即可自举提取——无需破译任何密钥。**

## 1. 侦察：鉴权门的确切形态

### 1.1 错误文案收敛（二进制 strings）

| 字符串 | 含义 |
|--------|------|
| `permission_denied:invalid LocalToken` | 403 家族文案 |
| `GinVerifyToken failed. token:%v header.LocalToken:%v header.Authorization:%v` | 校验器日志（header.* 为 Go struct 字段，**非** http.Header 键） |
| `Service.apiAuth find driveApiAllowLocalToken` | apiAuth 中间件查询 allowLocalToken 开关 |
| `token is expired. token:%v` / `token contains an invalid number of segments` | golang-jwt 错误族 → **token 为 JWT** |

实测：带 query 参数请求时 403 body 从 `permission…` 变为完整
`checkAuth failed:token contains an invalid number of segments token:`——
**token 字段为空**，即服务端未从我们的 header/query/body 中取到值，且它期望
三段式 JWT（golang-jwt `segments` 校验）。

### 1.2 KV 离线解密尝试（负结果，有方法论价值）

- 四个 storm/boltdb 库全部离线解析成功（`a3_boltdump.py`，自研 bbolt 遍历器）：
  - `user.core.db`：`device/device/{device_id,device_spaces,peer_id}`（密文 32-48B）
  - `6c8497ab…`：`PluginRuntime/INNER_API`（336B）
  - `cc7bb060…`（xllite）：`https://xluser-ssl.xunlei.com:X9ibISwpIp8jQ4Ya`（1056B，OAuth token 真身）、`docker.860599297.privilege`、`global-raw`（24944B）
- value 全部 **16B 对齐密文**；`__storm_db/version` 跨三库密文相同、user.core.db 不同
- 二进制 pclntab 定位：`/xunlei-pan-cli/pkg/aes/codec.go` 自研 codec——
  `AesCodec.Marshal/Unmarshal` + `NewECBEncrypter/Decrypter` + `PKCS5Padding` →
  **AES-ECB + PKCS5** 确证
- key 候选（`NcYbbjw1IyLXudeX` 16B 常量、device_id 各派生族 × ECB/CBC × 7 IV）全灭 →
  key 为运行期派生（未再深挖，见 §4 路线取舍）

## 2. 突破：web 前端自举链

引擎 `GET /` 返回内嵌 Vue 应用（vite 构建）。首页 HTML 4024B，其中引擎**动态注入**：

```html
<script>
  function uiauth(value){ return "eyJhbGciOiJIUzI1NiIs…NAEZp4PQEUkVeqZfC71KcPL2zD6Kqhq7TLBgNQ7lRYA" }
</script>
```

JWT payload（HS256）：

```json
{"key":"UIAuth","exp":1788396983,"iat":1788137783,"nbf":1788137783}
```

- `iat` = 引擎最近一次登录时刻；`exp = iat + 259200`（**3 天有效期**）
- 前端 JS（`assets/index-1ded6b9a.js`，1.5MB，已归档 evidence）HTTP 封装：

```js
u = await BVe();                    // BVe = () => window.uiauth(e)
headers: {"pan-auth": u, "Device-Space": i, …}
```

**→ 请求头 `pan-auth: <JWT>`。** 此前 9 种 header 猜测
（Authorization Bearer/raw、LocalToken×3、x-local-token、Cookie×2）全为 403，
`pan-auth` 一次命中。

## 3. 解锁后的 API 面全景（launcher 热启动 + pan-auth）

| 请求 | 状态 | 响应要点 |
|------|------|----------|
| `GET /drive/v1/tasks?page_token=&filters=` | **200** | 任务列表：`user#runner` 任务（`Docker-c-6a94bbe8…`）、`user_id=860599297` |
| `GET /device/v1/try_speed/get_info` | **200** | `{"status":0,"usage":{"total":3,"used":0},"count_down":{…},"task_counter":{"super_speed":null,…},"statistic":{…},"expire_sec":10}` |
| `POST /device/v1/try_speed/apply` | **200** | `{"message":"NO_RUNNING_TASK"}` —— 需有运行中任务才实际加速 |
| `POST /device/v1/try_speed/get_info` | 404 | 方法不匹配（与 A2 #9 一致：get_info=GET / apply=POST） |
| `GET /drive/v1/user/info` `storage/info` `events` `statistics` `setting` `history` | 404 | 路由未挂载（此版本引擎） |
| `GET /device/v1/info` `config` | 404 | 同上 |

**#10 终局定案**：try_speed 是设备内 API + 双路由（GET get_info / POST apply），
参数面以 `pan-auth` JWT 解锁；`usage.total=3` 为超级加速次数配额，
`apply` 的真实加速行为需在创建下载任务后验证（见 §5 遗留项）。

## 4. 路线取舍：为何不破 AES key

拿到 KV key 可离线读 OAuth token 真身（1056B 条目）与 INNER_API，但：

1. **业务闭环不需要**——`GET /` 自举 + `pan-auth` 已覆盖全部本地 API；
2. JWT 由引擎**持续自签**（每次渲染注入，iat 刷新），生命周期内永续可用；
3. AES key 运行期派生，破译成本高且收益仅剩「离线读凭据」一项（可由
   MITM 注入链在登录时同步捕获替代，A2 §3 已验证）。

`NcYbbjw1IyLXudeX`（16B）仍为可疑常量，其与 JWT HMAC secret / KV key 的
关系留给后续按需（如需离线伪造超长有效期 JWT 时再战）。

## 5. 遗留项（A4 候选）

| 项 | 说明 |
|----|------|
| 下载任务创建 + apply 加速实测 | `POST /drive/v1/tasks`（带 pan-auth）创建真实任务 → apply → 观察加速回包（涉及真实流量，待用户点头） |
| JWT 悬崖 | exp=iat+3d；引擎若停机 >3d 后重启，页面注入的 JWT 会重签吗（热启动是否刷新 iat）？实测路径：停机 3d 热启动。**已知启动即注入**，风险低 |
| tasks POST 载荷逆向 | 前端 JS 已归档，`Zt` 封装 + protobuf/JSON 形态可静态分析 |
| KV AES key | 见 §4，按需 |

## 6. 产物

| 资产 | 路径 |
|------|------|
| 突破校准器（终局） | `scripts/nas/a3_final.py` |
| JS 资产抓取器 | `scripts/nas/a3_boot.py` |
| BoltDB/storm 解析器 | `scripts/nas/a3_boltdump.py` |
| AES 密钥暴搜框架 | `scripts/nas/a3_keyhunt.py` |
| 原始证据 | `docs/nas/evidence/a3/`（a3_result_final.json、a3_boot.json、index.html 含 uiauth 注入原文） |
| 前端 JS bundle | evidence/a3/assets/（index-1ded6b9a.js 1.5MB 等 4 件，任务创建载荷逆向素材） |
