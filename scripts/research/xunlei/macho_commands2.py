import struct
import sys
from pathlib import Path
from macholib.MachO import MachO

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')

macho = MachO(str(BIN))

# macholib 的 commands 是 (header, cmd) 元组列表
print('=== 加载命令 ===')
for i, (header, cmd) in enumerate(macho.headers[0].commands):
    cmd_type = cmd[0] if isinstance(cmd, tuple) else getattr(cmd, 'cmd', '?')
    cmd_size = cmd[1] if isinstance(cmd, tuple) else getattr(cmd, 'cmdsize', '?')
    print(f'  [{i}] type={cmd_type}  size={cmd_size}')
