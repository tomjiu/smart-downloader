import struct
import sys
from pathlib import Path
from macholib.MachO import MachO

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')

macho = MachO(str(BIN))

# 打印所有加载命令类型
print('=== 所有加载命令 ===')
for i, item in enumerate(macho.headers[0].commands):
    # item 可能是 (load_command, seg_cmd, sections) 三元组
    if isinstance(item, tuple) and len(item) >= 2:
        lc = item[0]
        seg = item[1]
        print(f'  [{i}] cmd={lc.cmd}  seg={seg.segname}  size={lc.cmdsize}')
    else:
        print(f'  [{i}] type={type(item)}  len={len(item) if hasattr(item, "__len__") else "?"}')
