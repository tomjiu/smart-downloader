#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Scan the desktop Xunlei Chromium cache (data_1) for api-gateway-pan.xunlei.com
references and /xlppc.* paths. Pure stdlib, offline.

Outputs:
  - docs/research/xunlei/_gateway_scan.json  (machine readable)
  - prints a human summary to stdout
"""
import json
import os
import re
import sys
from collections import Counter, defaultdict

CACHE = os.path.expandvars(
    r"%APPDATA%\thunder\Cache\Cache_Data\data_1"
)
GATEWAY_HOST = "api-gateway-pan.xunlei.com"
XL_PREFIX = "/xlppc"

# URL-terminating characters (anything not allowed in a URL token).
_URL_TERM_BYTES = (
    bytes(range(0x00, 0x20))
    + b' "`<>\'\\^()[]{}|,;'
    + b"\x7f"
)
URL_TERM = set(_URL_TERM_BYTES)


def read_blob(path):
    with open(path, "rb") as f:
        return f.read()


def extract_urls(blob, host):
    """Return list of full URLs starting with https://<host>/..."""
    prefix = ("https://" + host).encode("latin-1")
    out = []
    start = 0
    while True:
        i = blob.find(prefix, start)
        if i < 0:
            break
        # extend until terminator
        j = i
        while j < len(blob) and blob[j] not in URL_TERM:
            j += 1
        url = blob[i:j].decode("latin-1", "ignore")
        out.append(url)
        start = j + 1
    return out


def extract_xlppc(blob):
    """Return list of /xlppc... paths found anywhere (not only after host)."""
    prefix = XL_PREFIX.encode("latin-1")
    out = []
    start = 0
    while True:
        i = blob.find(prefix, start)
        if i < 0:
            break
        j = i
        while j < len(blob) and blob[j] not in URL_TERM:
            j += 1
        path = blob[i:j].decode("latin-1", "ignore")
        out.append(path)
        start = j + 1
    return out


def window_auth_hits(blob, url_indices, window=256):
    """For each distinct URL offset, look in a window for auth-related strings."""
    auth_markers = [
        b"Authorization",
        b"authorization",
        b"x-captcha-token",
        b"X-Captcha-Token",
        b"Bearer ",
        b"x-client-id",
        b"x-device-id",
    ]
    hits = set()
    for idx in url_indices:
        lo = max(0, idx - window)
        hi = min(len(blob), idx + window)
        seg = blob[lo:hi]
        for m in auth_markers:
            if m in seg:
                hits.add(m.decode("latin-1"))
    return hits


def main():
    if not os.path.exists(CACHE):
        print(f"[fatal] cache file not found: {CACHE}", file=sys.stderr)
        sys.exit(1)
    blob = read_blob(CACHE)
    size = len(blob)

    urls = extract_urls(blob, GATEWAY_HOST)
    xlppc = extract_xlppc(blob)

    # Index offsets of each gateway URL for auth-window scan.
    prefix = ("https://" + GATEWAY_HOST).encode("latin-1")
    url_offsets = [m.start() for m in re.finditer(re.escape(prefix), blob)]

    auth_hits = window_auth_hits(blob, url_offsets, window=320)

    # Group full URLs by path (strip query/fragment for grouping, keep both counts).
    def path_of(u):
        # keep scheme+host+path; this collapses query-string variants.
        q = u.find("?")
        if q >= 0:
            u = u[:q]
        f = u.find("#")
        if f >= 0:
            u = u[:f]
        return u

    url_groups = Counter(path_of(u) for u in urls)
    xlppc_groups = Counter(xlppc)

    # Total raw references to the host (count of all occurrences of the host string).
    host_raw = blob.count(("https://" + GATEWAY_HOST).encode("latin-1"))

    out = {
        "cache_file": CACHE,
        "cache_size": size,
        "host": GATEWAY_HOST,
        "host_raw_references": host_raw,
        "distinct_full_urls": len(url_groups),
        "total_full_url_occurrences": len(urls),
        "distinct_xlppc_paths": len(xlppc_groups),
        "total_xlppc_occurrences": len(xlppc),
        "auth_markers_in_window": sorted(auth_hits),
        "url_groups": url_groups.most_common(),
        "xlppc_groups": xlppc_groups.most_common(),
    }
    os.makedirs("docs/research/xunlei", exist_ok=True)
    with open("docs/research/xunlei/_gateway_scan.json", "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, indent=2)

    print(f"cache size                : {size} bytes")
    print(f"host raw references       : {host_raw}")
    print(f"distinct full URLs        : {len(url_groups)}")
    print(f"total full URL occurrences: {len(urls)}")
    print(f"distinct /xlppc paths     : {len(xlppc_groups)}")
    print(f"total /xlppc occurrences  : {len(xlppc)}")
    print(f"auth markers in +/-320B   : {sorted(auth_hits) or 'NONE'}")
    print("\n== top 40 full URL paths ==")
    for path, c in url_groups.most_common(40):
        print(f"  {c:4d}  {path}")
    print("\n== top 40 /xlppc paths ==")
    for path, c in xlppc_groups.most_common(40):
        print(f"  {c:4d}  {path}")


if __name__ == "__main__":
    main()
