#!/usr/bin/env python3
# -*- coding: utf-8 -*-
import re, os
BIN = r"C:\Program Files\Thunder Network\Thunder\program\xllite.exe"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "oauth_pass4.txt")
probes = ["token-exchange", "client_credentials", "grant_type=refresh",
          "assertion", "scope=", "xluser-ssl.xunlei.com/oauth", "/oauth2/",
          "pan.xunlei.com/oauth", "openid", "id_token", "jwt",
          "Authorization: Bearer", "x-client-id", "XW5SkOhLDjnOZP7J",
          "api-pan.xunlei.com", "no client"]
with open(BIN, "rb") as f:
    data = f.read()
text = data.decode("utf-8", errors="replace")
out = []
for p in probes:
    hits = [m.start() for m in re.finditer(re.escape(p), text)]
    out.append(f"\n## {p!r} : {len(hits)} hits")
    for pos in hits[:5]:
        lo = max(0, pos - 140); hi = min(len(text), pos + 180)
        seg = re.sub(r"[\x00-\x1f]", ".", text[lo:hi])
        out.append(f"  @0x{pos:08X}: {seg}")
with open(OUT, "w", encoding="utf-8") as w:
    w.write("\n".join(out) + "\n")
print("done", len(out))
