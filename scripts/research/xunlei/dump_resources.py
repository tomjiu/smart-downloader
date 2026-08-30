#!/usr/bin/env python3
"""Dump all resources from XunLeiWebSetup25.0.90.1592xl11.exe (Xunlei 11 web setup)."""
import pefile, os, sys, hashlib

SRC = '/home/z/my-project/xunlei-25/XunLeiWebSetup25.0.90.1592xl11.exe'
OUT = '/home/z/my-project/xunlei-25/extracted'
os.makedirs(OUT, exist_ok=True)

pe = pefile.PE(SRC)

root = pe.DIRECTORY_ENTRY_RESOURCE
for t in root.entries:
    for n in t.directory.entries:
        for l in n.directory.entries:
            d = l.data.struct
            raw = pe.get_data(d.OffsetToData, d.Size)
            tname = pefile.RESOURCE_TYPE.get(t.id, str(t.id))
            tlabel = tname if isinstance(tname, str) else tname
            name = f'{tlabel}_{n.id if n.name is None else n.name}_{l.id if l.name is None else l.name}'
            fn = os.path.join(OUT, name + '.bin')
            with open(fn, 'wb') as f:
                f.write(raw)
            md5 = hashlib.md5(raw).hexdigest()[:12]
            print(f'{name:60s} size={d.Size:>9} md5={md5} magic={raw[:8].hex()} head={raw[:12]!r}')
