import struct
import sys
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

# 检查 vtable[0xb0] 处的内容
vtable = 0x7258e8
addr = vtable + 0xb0
q = struct.unpack_from('<Q', blob, addr)[0]
print(f'vtable[0xb0] at {addr:#x} = {q:#x}')

# 打印 vtable +0x80..+0x100 区域
print(f'\nvtable +0x80..+0x100:')
for off in range(0x80, 0x108, 8):
    q = struct.unpack_from('<Q', blob, vtable + off)[0]
    marker = ' <<<' if off == 0xb0 else ''
    print(f'  +{off:#04x}: {q:#018x}{marker}')

# 如果 q 在文件内，反汇编
if q < len(blob) and q > 0x1000:
    from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM
    md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)
    code = blob[q:q+0x200]
    print(f'\n=== 反汇编 {q:#x} ===')
    for insn in md.disasm(code, q):
        print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}')
        if insn.address > q + 0x100:
            break
