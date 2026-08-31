#!/usr/bin/env python3
"""A3-3b: AES key/pattern brute — 用 __storm_db/version 已知短明文形态验证."""
import hashlib, itertools, json, os, sys
from binascii import unhexlify

try:
    from Crypto.Cipher import AES
except ImportError:
    os.system('pip install pycryptodome -q')
    from Crypto.Cipher import AES

DEV_ID = 'c7d089aad73f7e2ddd2c263c2956b5a6'
PEER_ID = 'D6840C124CD7004V'
NC = b'NcYbbjw1IyLXudeX'

# 密文样本 (16B, 明文为短可读版本串)
CT = {
    'xllite-family': unhexlify('f85d9abb5ea2b0baa895fab5a8a1bc0e'),
    'user.core.db': unhexlify('500b8e2d2723abcbe8eb736ecf98f16f'),
}

def keys():
    k = {}
    k['NC'] = NC
    k['devid_bin'] = unhexlify(DEV_ID)
    k['devid_str'] = DEV_ID.encode()
    k['md5_devid_str'] = hashlib.md5(DEV_ID.encode()).digest()
    k['md5_devid_bin'] = hashlib.md5(unhexlify(DEV_ID)).digest()
    k['sha1_devid_str16'] = hashlib.sha1(DEV_ID.encode()).digest()[:16]
    k['sha256_devid_str'] = hashlib.sha256(DEV_ID.encode()).digest()
    k['sha256_devid_bin'] = hashlib.sha256(unhexlify(DEV_ID)).digest()
    k['sha256_NC'] = hashlib.sha256(NC).digest()
    k['NC*2'] = NC * 2
    k['md5_NC'] = hashlib.md5(NC).digest()
    k['peerid_str'] = PEER_ID.encode()
    k['md5_peerid'] = hashlib.md5(PEER_ID.encode()).digest()
    k['sha256_devid_str+NC'] = hashlib.sha256(DEV_ID.encode() + NC).digest()
    k['sha256_NC+devid_str'] = hashlib.sha256(NC + DEV_ID.encode()).digest()
    k['md5(NC+devid)'] = hashlib.md5(NC + DEV_ID.encode()).digest()
    k['md5(devid+NC)'] = hashlib.md5(DEV_ID.encode() + NC).digest()
    k['devid_str.upper'] = DEV_ID.upper().encode()
    k['sha256(version-tag)'] = hashlib.sha256(b'codecaes').digest()
    k['md5(codecaes)'] = hashlib.md5(b'codecaes').digest()
    k['md5(devid_str)+NC'] = hashlib.md5(DEV_ID.encode()).digest() + NC
    k['md5(NC+devid_str)16'] = hashlib.md5(NC + DEV_ID.encode()).digest()
    return k

IVS = lambda key: {
    'zero': b'\x00' * 16,
    'NC': NC,
    'devid_bin': unhexlify(DEV_ID),
    'key[:16]': key[:16],
    'md5(key)': hashlib.md5(key).digest(),
    'sha256(key)[:16]': hashlib.sha256(key).digest()[:16],
    'devid_bin_rev': unhexlify(DEV_ID)[::-1],
}

def score(pt):
    if not pt:
        return -1
    pad = pt[-1]
    ok_pad = 1 <= pad <= 16 and pt[-pad:] == bytes([pad]) * pad
    printable = sum(32 <= c < 127 for c in pt) / len(pt)
    return printable + (0.5 if ok_pad else 0)

def try_all(ct, label):
    best = []
    for kn, key in keys().items():
        if len(key) not in (16, 24, 32):
            continue
        # ECB
        pt = AES.new(key, AES.MODE_ECB).decrypt(ct)
        s = score(pt)
        if s > 0.5:
            best.append((s, kn, 'ECB', 'zero', pt))
        for ivn, iv in IVS(key).items():
            pt = AES.new(key, AES.MODE_CBC, iv=iv).decrypt(ct)
            s = score(pt)
            if s > 0.5:
                best.append((s, kn, 'CBC', ivn, pt))
    best.sort(reverse=True, key=lambda x: x[0])
    for s, kn, mode, ivn, pt in best[:6]:
        print(f'  [{label}] {kn:24s} {mode} iv={ivn:16s} score={s:.2f} pt={pt!r}')
    return best

if __name__ == '__main__':
    for label, ct in CT.items():
        print(f'=== target {label}: {ct.hex()}')
        b = try_all(ct, label)
        if not b:
            print('  (no hit)')
