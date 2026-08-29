import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

vtable_bt = 0x7258e8

# 读 vtable[0xb0]
val = struct.unpack_from('<Q', blob, vtable_bt + 0xb0)[0]
print(f'vtable[0xb0] = {val:#x}')

# 打印 vtable +0x80..+0xc0 区域（十六进制）
print(f'\nvtable +0x80..+0xc0:')
for off in range(0x80, 0xc8, 8):
    q = struct.unpack_from('<Q', blob, vtable_bt + off)[0]
    print(f'  +{off:#04x}: {q:#018x}')

# 如果 val 在文件内，反汇编
if val < len(blob):
    print(f'\n=== 反汇编 {val:#x} ===')
    code = blob[val:val+0x400]
    for insn in md.disasm(code, val):
        print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}')
