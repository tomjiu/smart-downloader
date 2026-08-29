#!/usr/bin/env python3
# -*- coding: utf-8 -*-
import os, re
CACHE = os.path.expandvars(r"%APPDATA%\thunder\Cache\Cache_Data\data_1")
blob = open(CACHE, "rb").read()

# Find the resinfo occurrence and show 200B before (to see if a host precedes it).
t = b"/xlppc.resinfo.api/v1/queryresinfo"
i = blob.find(t)
print("resinfo index:", i)
lo = max(0, i-120); hi = min(len(blob), i+200)
seg = blob[lo:hi]
printable = bytes(b if 32<=b<127 else 46 for b in seg)
print(printable.decode("latin-1"))

print("\n-- does 'https://api-gateway-pan' precede any /xlppc.resinfo? --")
# check window before
pre = blob[max(0,i-60):i]
print("bytes before (raw):", pre[-60:])

print("\n-- contexts of each distinct /xlppc path, 60B before, to see host --")
for p in [b"/xlppc.searcher.api/drive_common_search", b"/xlppc.searcher.api/drive_file_search", b"/xlppc.resinfo.api/v1/queryresinfo"]:
    j = blob.find(p)
    pre = blob[max(0,j-50):j]
    print(f"\n[{p.decode()}]")
    print("  before:", bytes(b if 32<=b<127 else 46 for b in pre).decode("latin-1"))
