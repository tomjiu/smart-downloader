# P2P 网络接入进展报告 v3

> **更新**: 2026-08-17
> **状态**: PHub HTTP body 加密格式已澄清（v2 MD5 公式仅适用于 XUDT/legacy；PHub 生产路径为 RSA-wrapped random AES key）

---

## 勘误（2026-08-27）

**v2 结论已局部作废**：本报告曾将 PHub HTTP body 加密描述为
`AES-ECB(MD5(cmd+seq), body)`（13 字节头 + 从包头派生 key）。
该公式**仅对 XUDT/legacy 路径成立**（见 `scripts/research/cloud_delivery/phub_line/XUDT_KEY_DERIVATION_SOLVED.md`）。

**PHub/SHub 生产路径正确模型**（v3 spec，已用真实样本验证）：
```
8 字节头 (cmd_id + flag + seq_no)
+ 4 字节 ekey_size (RSA-1024 = 128)
+ 128 字节 RSA-1024 密文 (包装随机 16B AES key)
+ 4 字节 aes_body_size
+ N 字节 AES-128-ECB 密文 (PKCS7)
```
- AES key = `XPF_RandomBytes(16)`，每请求随机
- RSA 公钥为编译期常量，服务端持私钥解出 AES key
- 离线密钥派生（MD5/seq）**不可能**；必须 Frida hook `XPF_RandomBytes` 或 `XPF_AESCreateEncryptContext` 才能拿到 key
- 规范文档：`scripts/research/cloud_delivery/v3/PHUB_PROTOCOL_SPEC_V3.md`

---

## 本轮进展

### 已确认 PHub 包格式 (A 级) — v2 头假设，仅 XUDT/legacy 适用

从反汇编 5 处 `call AES_ENCRYPT_FN` (0x180161920) 的调用者,确认:

```
PHub 包头 (13 字节, 不加密):
  [0:4]   cmd_id    (uint32 LE) — 值=1 (3 个调用者一致)
  [4]     flag       (uint8)    — 0xb(11) / 0x11(17) / 0x13(19) (不同命令不同)
  [5:9]   sequence   (uint32 LE) — 递增, 从全局变量读
  [9:13]  enc_len    (uint32 LE) — total_len - 13

PHub body ([13:], AES-ECB 加密):
  AES key = MD5([0:4] + [5:9])  ← 8 字节 → 16 字节
  加密前调 SERIALIZE_FN (0x1803192a0) 序列化 payload
  AES-ECB 原地加密 (XPF_AESEncryptBufferECB)
  PKCS7 padding
```
> ⚠️ **上式不适用于 PHub HTTP 生产包**。生产包见上方「勘误」v3 spec。

### 完整调用链 (A 级) — v2 路径

```
ServicePHubQueryEvent:
  1. rdx = [rdi+0x48]  ← PhubPkgRequester 对象
  2. rax = [rdx+0x28]   ← 包数据指针
  3. [rax+0] = 1         ← cmd_id
  4. [rax+4] = 0xb       ← flag
  5. [rax+5] = seq       ← sequence (递增)
  6. ecx = [rdx+0x30]   ← total_len
  7. ecx -= 0xd          ← enc_len = total_len - 13
  8. [rax+9] = ecx       ← 写 enc_len
  9. rdx = [rax+0x18]   ← 包数据基址
  10. rdx += 0xd         ← 加密源 = base + 13
  11. call SERIALIZE_FN  ← 序列化 payload
  12. rcx = [rdi+0x48]  ← PhubPkgRequester
  13. call AES_ENCRYPT   ← AES 加密 (key=MD5([0:4]+[5:9]))
  14. rcx = [rdi+0x38]  ← HTTP 请求对象
  15. rdx = [rdi+0x48]  ← PhubPkgRequester
  16. call HTTP_SEND    ← 发送 HTTP 请求 (0x1802833e0)
```

### 4 个 RSA-1024 公钥 (A 级)

```
公钥 #0 @ 0x3905e0 — ServerResource
公钥 #1 @ 0x394c40 — DPHubClient::LoginParent (需登录!)
公钥 #2 @ 0x395bf0 — CmdPHubQueryResResp::DoDecode ← PHub peer 查询!
公钥 #3 @ 0x396450 — PhubAllResHttpPkgTranslate ← 多资源查询!
全部: RSA-1024, e=65537
```

### captcha_sign 算法 (A 级, 已验证)

```python
def get_captcha_sign(timestamp_ms, device_id):
    s = f"{ClientID}{ClientVersion}{PackageName}{device_id}{timestamp_ms}"
    for algo in Algorithms:  # 10 个盐
        s = md5_hex(s + algo)
    return f"1.{s}"
```
- 匿名 captcha/init 不需要 captcha_sign
- 沙箱能拿到 captcha_token (300 秒有效)

### 服务器可达性 (A 级, 实测)

```
pr-phub.sandai.net       → 140.206.220.33 (TCP 80/443 ✓, POST / 返回 "decrypt request failed")
dcdnhub-xcloud.sandai.net → 140.206.225.182 (TCP 80/443 ✓, POST / 返回 "401 Decode error")
hub5btmain.sandai.net    → 112.64.218.154 (TCP 80/443 ✓, nginx 400)
dlcfg-pc-chub.sandai.net → 101.132.227.24 (TCP 80/443 ✓, 全部 404)
hub5p.sandai.net         → 127.0.0.2 (DNS 污染)
```

---

## 未解决的核心阻碍

### PHub HTTP body 加密格式 — 已澄清（v3 spec）

v2 的 14 个 PoC 全部失败，原因已定位：**生产路径不使用 MD5(seq) 派生 AES key**。

已尝试（v2 假设下）:
1. ✗ AES-ECB(MD5(cmd+seq), body) — 13B 明文头 + AES(body) — **仅 XUDT/legacy 适用**
2. ✗ RSA(AES key) + AES(body) — 4 公钥 × 2 padding
3. ✗ RSA(整个包) — PKCS1v15 / OAEP
4. ✗ base64 包装所有组合
5. ✗ 各种 HTTP header (captcha_token / device_id / client_id)
6. ✗ 各种 Content-Type
7. ✗ 各种 enc_len (明文长度 / 密文长度 / 0)
8. ✗ 各种 flag (0xb / 0x11 / 0x13 / 0-5)
9. ✗ 各种 cmd_id (0x22 / 0x19 / 0x1771 / 0-3)
10. ✗ 空 body / 1 byte / 128 byte / 256 byte
11. ✗ JSON 包装
12. ✗ Content-Encoding: gzip
13. ✗ 不带 enc_len (9 字节头)
14. ✗ 明文 body + header

**正确模型（v3 spec）**：
- 每请求生成 16B 随机 AES key (`XPF_RandomBytes`)
- AES key 用 RSA-1024 公钥加密后放在包体前部
- 包体用 AES-128-ECB + PKCS7 加密
- 规范文档：`scripts/research/cloud_delivery/v3/PHUB_PROTOCOL_SPEC_V3.md`

### 剩余开放问题

1. **HTTP 发送函数 (0x1802833e0) 的额外包装** — 即使 key 正确，仍需确认 HTTP body 是否被 chunked / 额外字符串包装
2. **真实抓包对照** — 需要 Wireshark 抓 Windows 上迅雷的 pr-phub.sandai.net:80 POST body，验证 v3 spec 的每一字段偏移
3. **XUDT 帧与 PHub HTTP 包的复用代码路径** — 确认 v2 代码路径是否仅用于 XUDT（UDP），PHub HTTP 是否走 `PhubPkgRequester::DoEncode` 的不同分支

### 下一步

1. **Frida hook XPF_RandomBytes / XPF_AESCreateEncryptContext** — 在 Thunder.exe 发起 PHub 请求时捕获真实 AES key
2. **反汇编 HTTP 发送函数 0x1802833e0** — 看它如何把 PhubPkgRequester 的加密包转成 HTTP body
3. **找 PhubPkgRequester 构造函数** — 看 this->0x48 (RSA context) 怎么创建,以及 this->0x28 怎么设置
4. **用户用 Wireshark 抓真实 PHub 流量** — 在 Windows 上跑迅雷,抓 pr-phub.sandai.net:80 的 HTTP POST body
