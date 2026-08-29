import struct
import sys
from pathlib import Path
from macholib.MachO import MachO
from macholib.mach_o import LC_REEXPORT_DYLIB, MH_CURRENT, MH_TARGET

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')

# 使用 macholib 解析 Mach-O
macho = MachO(str(BIN))

print('=== Mach-O 加载命令 ===')
for header, cmd in macho.headers[0].commands:
    print(f'  cmd={cmd.cmd}  size={cmd.cmdsize}')

# 找导出 trie 或符号表
for header, cmd in macho.headers[0].commands:
    cmd_name = cmd.cmd
    if cmd_name == 0x80000028:  # LC_DYLD_EXPORTS_TRIE
        print(f'\n=== 导出 Trie (LC_DYLD_EXPORTS_TRIE) ===')
        # 解析导出 trie 数据
        data_offset = cmd.data_offset
        data_size = cmd.data_size
        print(f'  offset={data_offset}  size={data_size}')
        
        with open(BIN, 'rb') as f:
            f.seek(data_offset)
            data = f.read(data_size)
        
        # 解析导出 trie 节点
        exports = parse_export_trie(data, 0, data_size, '', [])
        print(f'  找到 {len(exports)} 个导出符号')
        
        xl_exports = [(addr, name) for addr, name in exports if name.startswith('_XL')]
        print(f'\n=== _XL 前缀导出（共 {len(xl_exports)} 个）===')
        for addr, name in sorted(xl_exports, key=lambda x: x[1]):
            print(f'  {addr:#10x}  {name}')
