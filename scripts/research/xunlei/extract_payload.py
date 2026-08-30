#!/usr/bin/env python3
"""Extract the two 7z payloads from the new installer and inventory all PE modules."""
import py7zr, os, hashlib, struct

OUT = '/home/z/my-project/xunlei-25/payload'
os.makedirs(OUT, exist_ok=True)

for name, src in [('r203', '/home/z/my-project/xunlei-25/extracted/None_203_0.bin'),
                  ('r204', '/home/z/my-project/xunlei-25/extracted/None_204_0.bin')]:
    d = os.path.join(OUT, name)
    os.makedirs(d, exist_ok=True)
    with py7zr.SevenZipFile(src) as z:
        names = z.getnames()
        z.extractall(d)
    print(f'== {name} ({src}): {len(names)} entries ==')
    for n in sorted(names):
        print('   ', n)

print('\n== PE module versions ==')
def ver(fp):
    try:
        import pefile
        pe = pefile.PE(fp, fast_load=True)
        pe.parse_data_directories()
        info = {}
        for fi in getattr(pe, 'FileInfo', []) or []:
            for st in fi:
                if st.Key == b'StringFileInfo':
                    for tab in st.StringTable:
                        for k, v in tab.entries.items():
                            info[k.decode()] = v.decode()
        return info.get('FileVersion', '?'), info.get('FileDescription', '?'), info.get('ProductName', '?')
    except Exception as e:
        return 'not-pe', '', str(e)[:40]

for root, _, files in os.walk(OUT):
    for f in sorted(files):
        fp = os.path.join(root, f)
        sz = os.path.getsize(fp)
        fv, fd, pn = ver(fp)
        print(f'  {fp.replace(OUT+"/",""):70s} {sz:>9}  v={fv}  {fd[:40]}')
