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

# 修正：vtable 偏移是 0xb0（不是 0xb8）！
# BtTask vtable = 0x7258e8
vtable_bt = 0x7258e8
addr = struct.unpack_from('<Q', blob, vtable_bt + 0xb0)[0]
print(f'vtable[0xb0] = {addr:#x}')

# 也检查 0xb8 处（之前误读的）
addr_b8 = struct.unpack_from('<Q', blob, vtable_bt + 0xb8)[0]
print(f'vtable[0xb8] = {addr_b8:#x}')

if addr < len(blob):
    disasm(addr, 0x500, f'vtable[22] (0xb0) 填充函数')
