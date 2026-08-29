import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 打印前 100 条含 x1 的指令，看格式
count = 0
for insn in md.disasm(blob, 0):
    if 'x1' in insn.op_str:
        print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}')
        count += 1
        if count >= 40:
            break
