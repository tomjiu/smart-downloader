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

# 修正：vtable = 0x725000 + 0x8d8 + 0x10 = 0x7258e8
vtable_bt = 0x7258e8
print(f'BtTask vtable @ {vtable_bt:#x}')

# dump vtable 条目
print(f'\n=== BtTask vtable 条目 ===')
for off in range(0, 0x120, 8):
    a = struct.unpack_from('<Q', blob, vtable_bt + off)[0]
    if a == 0:
        continue
    valid = '✓' if a < len(blob) else '?'
    print(f'  +{off:#04x} [{off//8:2d}]: {a:#x} {valid}')

# vtable[0xb8/8] = vtable[23]
addr = struct.unpack_from('<Q', blob, vtable_bt + 0xb8)[0]
print(f'\nvtable[0xb8] = {addr:#x}')
if addr < len(blob):
    disasm(addr, 0x400, f'vtable[23] 填充函数')
