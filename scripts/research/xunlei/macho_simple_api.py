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
        print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}')
    print()

# 反汇编简单的 C 边界函数（只操作 task_id，不涉及复杂对象）
disasm(0x58e390, 0x100, '_XLStartTask')
disasm(0x58e4d0, 0x100, '_XLStopTask')
disasm(0x58e268, 0x100, '_XLReleaseTask')
disasm(0x58e138, 0x100, '_XLDeleteTask')

# 也看一些其他简单函数
disasm(0x58ddb8, 0x100, '_XLGetTaskIdList')
disasm(0x58df48, 0x100, '_XLGetTaskCount')
