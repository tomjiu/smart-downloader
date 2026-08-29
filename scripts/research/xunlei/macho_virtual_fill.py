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

# 0x580398 是 br x3（虚函数调用），反汇编它之后的代码看真实填充函数
disasm(0x580398, 0x180, '0x580398 虚函数目标之后')

# 同时看 0x580514（另一个 br x1 虚函数调用）
disasm(0x580514, 0x180, '0x580514 虚函数目标之后')
