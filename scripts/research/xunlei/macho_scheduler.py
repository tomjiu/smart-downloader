import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 0x502614 是 GetTaskInfo 的核心调度函数
# 完整反汇编 0x502638 分支（找到 task 后的路径）
code = blob[0x502638:0x502638+0x150]
print('=== 0x502638 GetTaskInfo 调度（找到 task 后的路径）===')
for insn in md.disasm(code, 0x502638):
    print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}')
