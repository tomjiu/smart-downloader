#!/usr/bin/env python3
"""XOR 不变量差分搜索：常量 XOR 不改变相邻差分，单遍扫描定位全部 XOR-k 变体."""
import sys

ENGINE = "/home/z/.nas-engine-test/data/.drive/bin/xunlei-pan-cli.3.23.5.amd64"
LAUNCHER = ("/home/z/my-project/repo-smart-downloader/scripts/research/xunlei/"
            "extracted/cross-platform/spk-x64/payload/bin/bin/xunlei-pan-cli-launcher.amd64")
TARGETS = [
    b"synoinfo.conf", b"authenticate.cgi", b"synos-release", b"/etc/VERSION",
    b"platform_name", b"OS_VERSION", b"SYNOPLATFORM", b"SYNOPKG_PKGNAME",
    b"unique=synology", b"\xe7\xbe\xa4\xe6\x99\x96",  # 群晖
    b"pan-xunlei-com", b"/var/packages",
]

def diff(data: bytes) -> bytes:
    n = len(data)
    return bytes(data[i] ^ data[i + 1] for i in range(n - 1))  # too slow in python?

def diff_fast(data: bytes) -> bytes:
    import numpy as np
    a = np.frombuffer(data, dtype=np.uint8)
    d = (a[:-1] ^ a[1:]).astype(np.uint8)
    return d.tobytes()

def search(path, name):
    data = open(path, "rb").read()
    D = diff_fast(data)
    print(f"=== {name} ({len(data)} bytes, diff {len(D)}) ===")
    for t in TARGETS:
        if len(t) < 4:
            continue
        Dt = bytes(t[i] ^ t[i + 1] for i in range(len(t) - 1))
        hits = []
        i = D.find(Dt)
        while i != -1 and len(hits) < 6:
            k = data[i] ^ t[0]
            hits.append((i, k))
            i = D.find(Dt, i + 1)
        if hits:
            for off, k in hits:
                ctx = data[max(0, off - 16):off + len(t) + 20]
                print(f"  {t.decode(errors='replace')!r:24} @{off} key=0x{k:02x} ctx={ctx!r}")
        else:
            print(f"  {t.decode(errors='replace')!r:24} -- none --")

if __name__ == "__main__":
    which = sys.argv[1] if len(sys.argv) > 1 else "both"
    if which in ("engine", "both"):
        search(ENGINE, "ENGINE pan-cli")
    if which in ("launcher", "both"):
        search(LAUNCHER, "LAUNCHER")
