import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)
md.detail = True

def disasm(addr, length, label):
    print(f'\n=== {label} @ {addr:#x} ===')
    code = blob[addr:addr+length]
    for insn in md.disasm(code, addr):
        # 高亮对 x1/x2（out 指针）的字段写入
        mark = ''
        if any(r in insn.op_str for r in ['x1', 'x2']) and any(op in insn.mnemonic for op in ['str', 'stp', 'ldr']):
            mark = '  <<< out 指针相关'
        print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}{mark}')
    print()

# GetTaskInfo 调用链：0x5168cc -> 0x502614 -> 0x580350
# 反汇编 0x580350（推测填充 TAG_XL_TASK_INFO_EX）
disasm(0x580350, 0x400, '0x580350 (GetTaskInfo 内部字段填充?)')
