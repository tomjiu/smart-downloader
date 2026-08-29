import struct
import sys
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

vtable = 0x7258e8

# 读 vtable[0xb0] 和 vtable[0xb8] 的原始值
b0 = struct.unpack_from('<Q', blob, vtable + 0xb0)[0]
b8 = struct.unpack_from('<Q', blob, vtable + 0xb8)[0]
print(f'vtable[0xb0] = {b0:#x}')
print(f'vtable[0xb8] = {b8:#x}')

# 打印 vtable +0x80..+0xc0 区域
print(f'\nvtable +0x80..+0xc0:')
for off in range(0x80, 0xc8, 8):
    q = struct.unpack_from('<Q', blob, vtable + off)[0]
    print(f'  +{off:#04x}: {q:#018x}')

# 检查 b8 处是否是函数地址（在文件范围内且有合理指令）
if b8 < len(blob):
    code = blob[b8:b8+4]
    from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM
    md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)
    insns = list(md.disasm(code, b8))
    if insns:
        print(f'\n{b8:#x} 反汇编: {insns[0].mnemonic} {insns[0].op_str}')
    else:
        print(f'\n{b8:#x} 无法反汇编，可能是数据')
