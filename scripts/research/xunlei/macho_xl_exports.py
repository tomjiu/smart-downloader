import struct
import sys
import re
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 搜索所有 _XL 开头的 C 符号字符串
print('=== 搜索所有 _XL 开头字符串 ===')
xl_pattern = re.compile(rb'_XL[A-Z][A-Za-z0-9_]+')

matches = []
for m in xl_pattern.finditer(blob):
    name = m.group(0).decode('ascii', 'ignore')
    offset = m.start()
    # 过滤掉太短的或明显不是函数名的
    if len(name) > 5 and not any(c in name for c in ['__', '::', '..']):
        matches.append((offset, name))

# 去重
seen = set()
unique = []
for offset, name in matches:
    if name not in seen:
        seen.add(name)
        unique.append((offset, name))

print(f'找到 {len(unique)} 个不重复的 _XL 字符串\n')

# 对于每个字符串，看它前面是否有函数入口模式
# 函数入口通常是: stp x29, x30, [sp, #-0x10]!  (ff 0f 00 b0  fd 7b 00 a9)
# 或者 sub sp, sp, #imm
func_start = re.compile(rb'(\xff\x0f\x00\xb0|\xfd\x7b\x00\xa9|\xfd\x7b\x01\xa9|\xfd\x7b\x02\xa9)')

print(f'{"地址":>10}  {"符号"}')
print('-' * 80)

xl_exports = []
for offset, name in sorted(unique, key=lambda x: x[1]):
    # 搜索字符串前面 64 字节内的函数入口
    search_start = max(0, offset - 64)
    search_area = blob[search_start:offset]
    
    # 找函数入口
    func_addr = None
    for m in func_start.finditer(search_area):
        func_addr = search_start + m.start()
        break
    
    if func_addr is not None:
        xl_exports.append((func_addr, name))
        print(f'  {func_addr:#10x}  {name}')

print(f'\n找到 {len(xl_exports)} 个可能的 _XL 导出函数')
