#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Contextual auth scan around api-gateway-pan URLs in the Desktop Xunlei cache.

- For each gateway URL occurrence, scan a wide window (+/- 2KB) for auth markers
  and report the nearest marker + offset.
- Report global counts of the markers in the whole file.
"""
import os
import re

CACHE = os.path.expandvars(
    r"%APPDATA%\thunder\Cache\Cache_Data\data_1"
)
HOST = b"https://api-gateway-pan.xunlei.com"
MARKERS = [
    b"Authorization",
    b"authorization",
    b"Bearer ",
    b"x-captcha-token",
    b"X-Captcha-Token",
    b"x-client-id",
    b"x-device-id",
    b"Pan-Auth",
    b"pan-auth",
    b"token",
    b"access_token",
    b"captcha_sign",
    b"device_sign",
]

blob = open(CACHE, "rb").read()
print(f"size: {len(blob)}")

# global counts
print("\n== global marker counts ==")
for m in MARKERS:
    c = blob.count(m)
    if c:
        print(f"  {m.decode('latin-1'):>18}: {c}")

offs = [m.start() for m in re.finditer(re.escape(HOST), blob)]
print(f"\n== {len(offs)} gateway occurrences; nearest auth marker within +/-2KB ==")
W = 2048
found_any = False
for idx in offs:
    lo = max(0, idx - W)
    hi = min(len(blob), idx + W)
    seg = blob[lo:hi]
    nearest = None
    for m in MARKERS:
        p = seg.find(m)
        if p >= 0:
            glob = lo + p
            delta = glob - idx
            if nearest is None or abs(delta) < abs(nearest[1]):
                nearest = (m.decode("latin-1"), delta)
    if nearest:
        found_any = True
        # only print a handful to keep output small
        pass

# Print context for the first drive_common_search occurrence
target = b"xlppc.searcher.api/drive_common_search"
ti = blob.find(target)
if ti >= 0:
    print("\n== context around first drive_common_search (256B before, 256B after) ==")
    lo = max(0, ti - 256)
    hi = min(len(blob), ti + 512)
    seg = blob[lo:hi]
    # make printable
    printable = bytes(b if 32 <= b < 127 else 46 for b in seg)
    print(printable.decode("latin-1"))

print("\nany auth marker within +/-2KB of any gateway URL:", found_any)
