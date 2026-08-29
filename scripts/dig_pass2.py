#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Pass 2: 从 xllite.exe 提取真实 Xunlei OAuth 交换端点 / 参数 / host.
- 所有 https?:// URL (去重, 统计 xunlei.com 子域)
- xunlei.com 相关的 oauth / passport / token / sso 路径
- XW5SkOhLDjnOZP7J 的全部真实上下文 (排除 config-list 噪声)
- grant_type / authorization_code / refresh_token / client_secret 等参数串
"""
import re
import os

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "oauth_pass2.txt")


def main():
    with open(BIN, "rb") as f:
        data = f.read()
    text = data.decode("utf-8", errors="replace")

    with open(OUT, "w", encoding="utf-8") as w:
        def emit(s):
            w.write(s + "\n")

        # 1) 所有 URL
        emit("#" * 76)
        emit("#  ALL URLs (http/https) 去重")
        emit("#" * 76)
        urls = re.findall(rb"https?://[A-Za-z0-9_.:/?=&%@~#\-]+", data)
        uni = {}
        for u in urls:
            try:
                s = u.decode("utf-8", errors="replace")
            except Exception:
                continue
            uni[s] = uni.get(s, 0) + 1
        # 按 host 分组统计
        host_count = {}
        for s, c in uni.items():
            m = re.match(r"https?://([^/]+)", s)
            if m:
                h = m.group(1)
                host_count[h] = host_count.get(h, 0) + c
        emit("\n## Host 频率 Top (xunlei 相关):")
        for h, c in sorted(host_count.items(), key=lambda kv: -kv[1]):
            if "xunlei" in h.lower() or "oauth" in h.lower() or "passport" in h.lower() or "xluser" in h.lower():
                emit(f"  {c:6d}  {h}")
        emit("\n## 含 oauth/passport/token/sso 的 URL 样例:")
        seen = set()
        for s, c in uni.items():
            if re.search(r"oauth|passport|/token|/sso|authorize", s, re.I):
                if s in seen:
                    continue
                seen.add(s)
                emit(f"  ({c}x) {s[:240]}")

        # 2) xunlei.com 相关路径 (任意字符串里出现 xunlei.com 且带 oauth/token/sso/passport)
        emit("\n" + "#" * 76)
        emit("#  xunlei.com 上下文 (oauth/token/sso/passport/authorize) ±250")
        emit("#" * 76)
        for kw in ["oauth", "token", "sso", "passport", "authorize", "credential", "x/oauth", "yc/oauth", "pkg/oauth"]:
            for m in re.finditer(re.escape(kw), text, re.I):
                lo = max(0, m.start() - 250)
                hi = min(len(text), m.end() + 250)
                seg = text[lo:hi]
                if "xunlei" in seg.lower():
                    emit(f"\n--- kw={kw!r} @0x{m.start():08X} ---")
                    emit(re.sub(r"[\x00-\x1f]", ".", seg))
                    break  # 每个 kw 只看第一个含 xunlei 的上下文即可示意
            else:
                emit(f"\n[kw={kw!r}] 无含 xunlei 的上下文")

        # 3) XW5SkOhLDjnOZP7J 全部出现位置 (±300), 标出是否为 config-list
        emit("\n" + "#" * 76)
        emit("#  XW5SkOhLDjnOZP7J 全部上下文 (±300)  识别是否仅 config-list")
        emit("#" * 76)
        cnt = 0
        for m in re.finditer(r"XW5SkOhLDjnOZP7J", text):
            lo = max(0, m.start() - 300)
            hi = min(len(text), m.end() + 300)
            seg = text[lo:hi]
            is_cfg = ("x-client-id" in seg) or ("match" in seg and "desc" in seg)
            cnt += 1
            emit(f"\n=== XW5Sk @0x{m.start():08X} (config-list? {is_cfg}) ===")
            emit(re.sub(r"[\x00-\x1f]", ".", seg))
        emit(f"\n[XW5Sk 总计 {cnt} 处]")

        # 4) 关键参数串字面量
        emit("\n" + "#" * 76)
        emit("#  令牌交换参数 / 字段 字面量搜索")
        emit("#" * 76)
        params = ["grant_type", "authorization_code", "refresh_token",
                  "client_secret", "client_id", "response_type", "access_token",
                  "id_token", "device_code", "user_code", "sso_ticket", "ticket",
                  "x-client-id", "x-client-type", "x-device-id"]
        for p in params:
            positions = [m.start() for m in re.finditer(re.escape(p), text)]
            emit(f"\n## {p!r} : {len(positions)} hits")
            for pos in positions[:6]:
                lo = max(0, pos - 120)
                hi = min(len(text), pos + 160)
                seg = text[lo:hi]
                emit(f"  @0x{pos:08X}: ...{re.sub(chr(0x00)+'-'+chr(0x1f),'.',seg)}...")

        # 5) 'x/oauth2' 作为真实路径 (前面是 host, 后面是路径) 而非 golang.org/x/oauth2
        emit("\n" + "#" * 76)
        emit("#  '/x/oauth2' 真实路径上下文 (排除 golang.org/x/oauth2 与 /pkg/oauth2client 符号)")
        emit("#" * 76)
        for m in re.finditer(r"/x/oauth2", text):
            seg = text[max(0, m.start() - 80): m.end() + 200]
            if "golang.org" in seg or "oauth2client" in seg or "gorm" in seg or "internal" in seg[:10]:
                continue
            emit(f"\n@0x{m.start():08X}: {re.sub(chr(0x00)+'-'+chr(0x1f),'.',seg)}")

        emit("\n[done]")


if __name__ == "__main__":
    main()
