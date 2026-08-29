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

# 1. _XL_InitDownloadLib (C 边界) — 看初始化参数结构体
disasm(0x5f86ac, 0x300, '_XL_InitDownloadLib (C 边界)')

# 2. 0x51a93c (_XLGetGlobalDownloadSpeed 的尾调用目标)
disasm(0x51a93c, 0x400, '0x51a93c (speed impl)')
