import struct
import sys
import re
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

symoff = 0x7ddb18
nsyms = 94892
stroff = 0x951be0
strsize = 0x152b40

# 收集所有 external 符号
ext_syms = []
for i in range(nsyms):
    ent = symoff + i * 16
    n_strx = struct.unpack_from('<I', blob, ent)[0]
    n_type = struct.unpack_from('<B', blob, ent + 4)[0]
    n_sect = struct.unpack_from('<B', blob, ent + 5)[0]
    n_value = struct.unpack_from('<Q', blob, ent + 8)[0]
    if n_strx and (n_type & 0x01):
        end = blob.find(b'\x00', stroff + n_strx)
        sym = blob[stroff + n_strx:end].decode('ascii', 'ignore')
        ext_syms.append((sym, n_type, n_sect, n_value))

print(f'external 符号: {len(ext_syms)} 个\n')

# 分类
def show(pattern, label, limit=60):
    hits = [(s, v) for s, t, sec, v in ext_syms if pattern in s]
    print(f'=== {label}（{len(hits)} 个）===')
    for s, v in sorted(hits)[:limit]:
        print(f'  {s}')
    if len(hits) > limit:
        print(f'  ... 共 {len(hits)} 个')
    print()

show('DownloadLib', '含 DownloadLib')
show('CreateBt', '含 CreateBt')
show('CreateTask', '含 CreateTask')
show('XLCreate', '含 XLCreate')
show('CreateP2sp', '含 CreateP2sp')
show('CreateMagnet', '含 CreateMagnet')
show('GetTaskInfo', '含 GetTaskInfo')
show('Init', '含 Init')
