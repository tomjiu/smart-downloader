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

# C 导出符号 = 以 _ 开头，且后面不是 _Z（C++ mangled 是 __Z）
c_exports = []
for sym, n_type, n_sect, n_value in ext_syms:
    # 排除 C++ mangled（__Z 开头）和 Swift（_$s 开头）
    if sym.startswith('__Z') or sym.startswith('_$s'):
        continue
    if sym.startswith('_'):
        c_exports.append((sym[1:], n_type, n_sect, n_value))

print(f'C 风格导出符号（非 C++ mangled）: {len(c_exports)} 个\n')

# 重点：XL 开头
xl_c = [(s, t, sec, v) for s, t, sec, v in c_exports if s.startswith('XL')]
print(f'=== XL 开头 C 导出符号: {len(xl_c)} 个 ===')
for s, t, sec, v in sorted(xl_c):
    print(f'  {s}')

# 其他非 XL 但可能是 API 的（看前 100 个 C 导出）
print(f'\n=== 其他 C 导出符号（前 150 个，按名排序）===')
others = sorted(c_exports, key=lambda x: x[0])
for s, t, sec, v in others[:150]:
    if not s.startswith('XL'):
        print(f'  {s}')
