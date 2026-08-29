#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Pass 3: 确认 xllite 真实令牌流 & 是否存在第二段交换.
- 设备码授权端点 (device_code / device/code / verification_uri)
- xllite:access_token / xllite:token_secret 上下文
- Xqp0 全量 (票名 or client id?)
- redirect_uri 指向 xunlei (非 aliyundrive) 的
- xluser-ssl 全部 URL + 路径
- 'no client info found' 上下文
- login / passport / getToken / session 的真实路径
"""
import re
import os

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "oauth_pass3.txt")


def extract(text, needle, ctx, out, label=None, limit=None, filter_fn=None):
    idxs = [m.start() for m in re.finditer(re.escape(needle), text)]
    shown = idxs if not limit else idxs[:limit]
    for i, pos in enumerate(shown):
        if filter_fn and filter_fn(text, pos):
            continue
        lo = max(0, pos - ctx)
        hi = min(len(text), pos + len(needle) + ctx)
        seg = text[lo:hi]
        seg = re.sub(r"[\x00-\x1f]", ".", seg)
        tag = label or needle
        out.append(f"\n=== {tag} @0x{pos:08X} ({i+1}/{len(idxs)}) ===")
        out.append(seg)


def main():
    with open(BIN, "rb") as f:
        data = f.read()
    text = data.decode("utf-8", errors="replace")
    out = []

    out.append("#" * 76)
    out.append("#  device_code 授权流端点")
    out.append("#" * 76)
    extract(text, "urn:ietf:params:oauth:grant-type:device_code", 500, out)
    extract(text, "verification_uri", 300, out)
    extract(text, "device_code", 200, out, limit=4)

    out.append("\n" + "#" * 76)
    out.append("#  xllite token 存储键")
    out.append("#" * 76)
    extract(text, "xllite:access_token", 400, out)
    extract(text, "xllite:token_secret", 400, out)

    out.append("\n" + "#" * 76)
    out.append("#  Xqp0 全量出现 (票名 or client id?)")
    out.append("#" * 76)
    extract(text, "Xqp0", 300, out)

    out.append("\n" + "#" * 76)
    out.append("#  redirect_uri 指向 xunlei (非 aliyundrive)")
    out.append("#" * 76)
    for m in re.finditer(r"redirect_uri", text):
        seg = text[max(0, m.start() - 200): m.end() + 250]
        if "xunlei" in seg and "aliyundrive" not in seg:
            out.append(f"\n=== redirect_uri @0x{m.start():08X} ===")
            out.append(re.sub(r"[\x00-\x1f]", ".", seg))

    out.append("\n" + "#" * 76)
    out.append("#  xluser-ssl.xunlei.com 全部 URL")
    out.append("#" * 76)
    urls = set()
    for m in re.finditer(r"https?://xluser-ssl\.xunlei\.com[^\s\"'<>]*", text):
        urls.add(m.group(0))
    for u in sorted(urls):
        out.append(f"  {u[:200]}")

    out.append("\n" + "#" * 76)
    out.append("#  'no client info found' 上下文")
    out.append("#" * 76)
    extract(text, "no client info found", 400, out)
    extract(text, "client info", 300, out, limit=4)

    out.append("\n" + "#" * 76)
    out.append("#  login / passport / token / session 真实路径 (含 host.xunlei 且像 API)")
    out.append("#" * 76)
    for kw in ["/login", "/passport", "/user/token", "/oauth/token", "getToken",
               "getUserToken", "exchange", "authorize?", "device/code", "device_code"]:
        hits = [m.start() for m in re.finditer(re.escape(kw), text)]
        if not hits:
            out.append(f"\n[kw={kw!r}] 0 hits")
            continue
        out.append(f"\n## {kw!r} : {len(hits)} hits (示例前3)")
        for pos in hits[:3]:
            lo = max(0, pos - 150)
            hi = min(len(text), pos + 200)
            seg = re.sub(r"[\x00-\x1f]", ".", text[lo:hi])
            out.append(f"  @0x{pos:08X}: {seg}")

    out.append("\n" + "#" * 76)
    out.append("#  'access_token' 真实调用上下文 (排除 struct 字段 & 标点表)")
    out.append("#" * 76)
    cnt = 0
    for m in re.finditer(r"access_token", text):
        seg = text[max(0, m.start() - 100): m.end() + 150]
        # 过滤掉纯标点/字段名噪声
        if re.search(r"json:|protobuf:|is empty|missing", seg):
            continue
        cnt += 1
        if cnt <= 8:
            out.append(f"\n@0x{m.start():08X}: {re.sub(chr(0x00)+'-'+chr(0x1f),'.',seg)}")
    out.append(f"\n[access_token 有效上下文 {cnt} 处]")

    with open(OUT, "w", encoding="utf-8") as w:
        w.write("\n".join(out) + "\n")
    print("wrote", OUT, "lines", len(out))


if __name__ == "__main__":
    main()
