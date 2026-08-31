#!/usr/bin/env python3
"""A3-3: BoltDB(storm) 库结构解析器 v2 — dump bucket 树 + KV 原文/密文形态.
实测校准: magic LE bytes edda0ced; page flags: branch=0x01 leaf=0x02 meta=0x04 freelist=0x10;
meta page = 16B page header + meta struct(magic,version,pageSize,flags,root{pgid,seq},freelist,pgid,txid,checksum)
leaf element: <IIII> flags,pos,ksz,vsz; element flag 0x01 = bucket child (value = root pgid + seq)
"""
import struct, sys, os, glob

PAGE_META, PAGE_LEAF, PAGE_BRANCH, PAGE_FREELIST = 0x04, 0x02, 0x01, 0x10
ELEM_BUCKET = 0x01


class Bolt:
    def __init__(self, path):
        self.data = open(path, 'rb').read()
        self.pgsz = 4096
        meta = self._read_meta(1) or self._read_meta(0)
        if not meta:
            raise ValueError('no valid meta')
        _, self.root, _, txid = meta
        self.txid = txid

    def _read_meta(self, pgid):
        off = pgid * self.pgsz
        if off + 80 > len(self.data):
            return None
        pg_pid, pg_flags, pg_cnt, pg_ovf = struct.unpack_from('<QHHI', self.data, off)
        if pg_flags != PAGE_META:
            return None
        magic, version, pgsz, _fl = struct.unpack_from('<IIII', self.data, off + 16)
        if magic != 0xED0CDAED:
            return None
        root_pgid, root_seq = struct.unpack_from('<QQ', self.data, off + 32)
        _freelist, _mpgid, txid = struct.unpack_from('<QQQ', self.data, off + 48)
        self.pgsz = pgsz
        return True, root_pgid, pgsz, txid

    def page(self, pgid):
        off = pgid * self.pgsz
        pid, flags, cnt, ovf = struct.unpack_from('<QHHI', self.data, off)
        body = off + 16
        size = self.pgsz - 16
        raw = self.data[body:body + size]
        if ovf:  # append overflow pages
            nxt = pgid + 1
            for i in range(ovf):
                raw += self.data[nxt * self.pgsz:(nxt + 1) * self.pgsz]
                nxt += 1
        return flags, cnt, raw

    def walk(self, pgid, path, out, seen_pages):
        if pgid == 0 or pgid * self.pgsz >= len(self.data) or pgid in seen_pages:
            return
        seen_pages.add(pgid)
        try:
            flags, cnt, raw = self.page(pgid)
        except Exception:
            return
        if flags == PAGE_LEAF:
            base = pgid * self.pgsz + 16  # page body start (element headers live here)
            for i in range(cnt):
                hdr_off = base + i * 16
                if hdr_off + 16 > len(self.data):
                    break
                eflags, pos, ksz, vsz = struct.unpack_from('<IIII', self.data, hdr_off)
                k = self.data[hdr_off + pos: hdr_off + pos + ksz]
                v = self.data[hdr_off + pos + ksz: hdr_off + pos + ksz + vsz]
                if eflags & ELEM_BUCKET and vsz >= 16:
                    child_root = struct.unpack_from('<Q', v, 0)[0]
                    name = k.decode('utf-8', 'replace')
                    self.walk(child_root, path + name + '/', out, seen_pages)
                else:
                    out.append((path, k, v))
        elif flags == PAGE_BRANCH:
            off = 0
            kids = []
            for _ in range(cnt):
                if off + 16 > len(raw):
                    break
                _pos, ksz, pg = struct.unpack('<IIQ', raw[off:off + 16])
                off += 16
                kids.append(pg)
            for pg in kids:
                self.walk(pg, path, out, seen_pages)


def shape(v):
    n = len(v)
    printable = sum(32 <= c < 127 for c in v) / max(n, 1)
    return f'len={n} pr={printable:.0%} {v[:24].hex()}'


if __name__ == '__main__':
    targets = sys.argv[1:] or sorted(
        glob.glob(os.path.expanduser('~/.nas-engine-test/data/.drive/*')))
    for path in targets:
        if not os.path.isfile(path):
            continue
        try:
            b = Bolt(path)
        except Exception as e:
            continue
        print(f'=== {os.path.basename(path)}  txid={b.txid}')
        out, seen = [], set()
        b.walk(b.root, '', out, seen)
        dedup = set()
        for p, k, v in out:
            ks = k.decode('utf-8', 'replace')
            sig = (p, ks, shape(v))
            if sig in dedup:
                continue
            dedup.add(sig)
            try:
                vv = v.decode('utf-8')
                if vv.isprintable():
                    print(f'  {p}{ks} -> PLAIN "{vv[:80]}"')
                    continue
            except UnicodeDecodeError:
                pass
            print(f'  {p}{ks} -> {shape(v)}')
        print()
