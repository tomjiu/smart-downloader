#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
定向静态考古: xllite.exe OAuth/SSO 第二段令牌交换链路还原.
只处理 UTF-8 明文字符串(Go 二进制). 一次读入多次正则.
修复: 直接以 utf-8 写文件, 避免 GBK 控制台编码崩溃.
"""
import re
import os
import sys

BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "oauth_dump.txt")

PRIMARY_ANCHORS = [
    "/x/oauth2",
    "/x/oauth2/internal",
    "/yc/oauth",
    "/o/oauth/authorize",
    "/pkg/oauth2client",
    "X9ibISwpIp8jQ4Ya",
    "XVJVzaJv8vKHzVCk",
]

SECONDARY_ANCHORS = [
    "sso",
    "ticket",
    "code_link",
    "authorizePage",
    "credentials_",
    "Xqp0kJBXWhwaTpB6",
    "redirect_uri",
]

SECONDARY_LIMIT = {
    "Xqp0kJBXWhwaTpB6": 6,
}

CONTEXT_PRIMARY = 400
CONTEXT_SECONDARY = 200


def find_all(data, needle):
    out = []
    nb = needle.encode("utf-8")
    start = 0
    while True:
        idx = data.find(nb, start)
        if idx == -1:
            break
        out.append(idx)
        start = idx + 1
    return out


def context_text(data, idx, ctx, needle):
    lo = max(0, idx - ctx)
    hi = min(len(data), idx + len(needle.encode("utf-8")) + ctx)
    frag = data[lo:hi]
    try:
        txt = frag.decode("utf-8", errors="replace")
    except Exception:
        txt = frag.decode("latin-1", errors="replace")
    clean = re.sub(r"[\x00-\x08\x0b\x0c\x0e-\x1f]", ".", txt)
    return lo, hi, clean


def main():
    with open(BIN, "rb") as f:
        data = f.read()
    with open(OUT, "w", encoding="utf-8") as w:
        w.write(f"[*] read {len(data)} bytes\n")

        def emit(s):
            w.write(s + "\n")

        emit("")
        emit("#" * 76)
        emit(f"#  PRIMARY ANCHORS  (+/-{CONTEXT_PRIMARY})")
        emit("#" * 76)
        for a in PRIMARY_ANCHORS:
            positions = find_all(data, a)
            emit(f"\n### PRIMARY {a!r} : {len(positions)} hits")
            for p in positions:
                lo, hi, clean = context_text(data, p, CONTEXT_PRIMARY, a)
                emit("")
                emit("=" * 76)
                emit(f"[PRIMARY] offset=0x{p:08X} ({p})  needle={a!r}")
                emit(f"ctx={CONTEXT_PRIMARY}B  window=0x{lo:08X}..0x{hi:08X}")
                emit("-" * 76)
                emit(clean)
                emit("=" * 76)

        emit("")
        emit("#" * 76)
        emit(f"#  SECONDARY ANCHORS  (+/-{CONTEXT_SECONDARY})")
        emit("#" * 76)
        for a in SECONDARY_ANCHORS:
            positions = find_all(data, a)
            limit = SECONDARY_LIMIT.get(a)
            shown = positions[:limit] if limit else positions
            emit(f"\n### SECONDARY {a!r} : {len(positions)} hits" + (f" (showing first {limit})" if limit else ""))
            for p in shown:
                lo, hi, clean = context_text(data, p, CONTEXT_SECONDARY, a)
                emit("")
                emit("=" * 76)
                emit(f"[SECONDARY] offset=0x{p:08X} ({p})  needle={a!r}")
                emit(f"ctx={CONTEXT_SECONDARY}B  window=0x{lo:08X}..0x{hi:08X}")
                emit("-" * 76)
                emit(clean)
                emit("=" * 76)

        # 额外 host / param 线索
        host_pats = ["xunlei.com", "xluser", "pan.xunlei", "client_id", "grant_type",
                     "response_type", "refresh_token", "access_token", "redirect_uri",
                     "authorize", "o/oauth", "x/oauth", "yc/oauth", "Xqp0",
                     "code_link", "authorizePage", "sso", "ticket", "credentials_"]
        emit("")
        emit("#" * 76)
        emit("#  EXTRA HOST/PARAM CLUES")
        emit("#" * 76)
        for hp in host_pats:
            positions = find_all(data, hp)
            emit(f"\n### HOSTCLUE {hp!r} : {len(positions)} hits (showing first 10)")
            for p in positions[:10]:
                lo, hi, clean = context_text(data, p, 160, hp)
                emit("")
                emit("-" * 76)
                emit(f"[HOSTCLUE] offset=0x{p:08X} ({p})  needle={hp!r}")
                emit(clean)

        emit("\n[done]")


if __name__ == "__main__":
    main()
