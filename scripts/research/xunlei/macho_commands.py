import struct
import sys
from pathlib import Path
from macholib.MachO import MachO

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')

macho = MachO(str(BIN))

# 打印所有加载命令的详细信息
for header in macho.headers:
    for cmd in header.commands:
        cmd_type = cmd.cmd
        cmd_size = cmd.cmdsize
        
        # 检查是否是导出相关的命令
        if cmd_type in (0x80000028, 0x80000029):  # LC_DYLD_EXPORTS_TRIE, LC_DYLD_CHAINED_FIXUPS
            print(f'\n=== 命令 {cmd_type:#x} (size={cmd_size}) ===')
            for attr in dir(cmd):
                if not attr.startswith('_'):
                    try:
                        val = getattr(cmd, attr)
                        if not callable(val):
                            print(f'  {attr}: {val}')
                    except Exception:
                        pass
        
        # 打印符号表命令
        if hasattr(cmd, 'symtab'):
            print(f'\n=== 符号表命令 ===')
            for attr in dir(cmd):
                if not attr.startswith('_'):
                    try:
                        val = getattr(cmd, attr)
                        if not callable(val):
                            print(f'  {attr}: {val}')
                    except Exception:
                        pass
