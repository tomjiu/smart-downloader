import struct
import sys
import re
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

magic = struct.unpack_from('<I', blob, 0)[0]
ncmds = struct.unpack_from('<I', blob, 16)[0]
sizeofcmds = struct.unpack_from('<I', blob, 20)[0]

# 遍历 load commands，找是否有调试信息（LC_DYSYMTAB 里的 info 等）
print('=== Load Commands ===')
cmd_names = {
    0x1:'SEGMENT_64', 0x2:'SYMTAB', 0xb:'DYSYMTAB', 0x80000022:'DYLD_INFO',
    0x80000023:'DYLD_INFO_ONLY', 0x80000028:'MAIN', 0x19:'SEGMENT_64',
    0x8000001e:'RPATH', 0xc:'LOAD_DYLIB', 0xd:'ID_DYLIB',
    0x1b:'UUID', 0x80000022:'DYLD_INFO', 0x2a:'CODE_SIGNATURE',
    0x8000001f:'REEXPORT_DYLIB', 0x1d:'LOAD_WEAK_DYLIB',
    0x80000024:'FUNCTION_STARTS', 0x80000025:'DATA_IN_CODE',
}

off = 32
for i in range(ncmds):
    cmd = struct.unpack_from('<I', blob, off)[0]
    cmdsize = struct.unpack_from('<I', blob, off + 4)[0]
    name = cmd_names.get(cmd, f'cmd_{hex(cmd)}')
    print(f'  [{i}] {name} (size {cmdsize})')
    off += cmdsize

# 检查是否有 .debug 段（DWARF）
print('\n=== 搜索 DWARF 特征 ===')
if b'.debug_info' in blob or b'__debug_info' in blob:
    print('  找到 .debug_info！')
if b'DWARF' in blob or b'dwarf' in blob:
    print('  找到 DWARF 字符串')
# 检查 __DWARF segment
if b'__DWARF' in blob:
    print('  找到 __DWARF segment（调试信息保留！）')
else:
    print('  未找到 __DWARF segment（调试信息被剥离）')
