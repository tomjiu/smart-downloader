import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

def disasm(addr, length, label):
    print(f'\n=== {label} @ {addr:#x} ===')
    code = blob[addr:addr+length]
    for insn in md.disasm(code, addr):
        print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}')
    print()

# 反汇编 DownloadLib::CreateBtMagnetTask — 对比 BT，磁力参数布局
disasm(0x519414, 0x180, 'DownloadLib::CreateBtMagnetTask(TAG_TASK_PARAM_MAGNET*, u64*)')

# 反汇编 DownloadLib::GetTaskInfo — 看 TAG_XL_TASK_INFO_EX 的字段
disasm(0x5168cc, 0x180, 'DownloadLib::GetTaskInfo(u64, TAG_XL_TASK_INFO_EX*)')
