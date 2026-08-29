import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)
md.detail = True

# 补全 0x580458-0x580480（找到节点后，调用虚函数填充 out 的关键路径）
print('=== 0x580458 找到节点后填充 out ===')
code = blob[0x580458:0x580480]
for insn in md.disasm(code, 0x580458):
    print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}')

# 同时搜索 vtable+0xb8 的写入（可能在 .rodata 或构造函数里）
# 搜索整个二进制中的 ldr x3, [x8, #0xb8]（读取虚函数），看附近有没有线索
print('\n=== 搜索 ldr x3, [x8, #0xb8]（虚函数调用模式）===')
count = 0
for insn in md.disasm(blob, 0):
    if insn.mnemonic == 'ldr' and 'x3, [x8, #0xb8]' in insn.op_str:
        print(f'  {insn.address:#x}: {insn.op_str}')
        count += 1
        if count >= 10:
            break
