import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 先确认 capstone 能反汇编（从已知函数地址开始）
print('=== 测试反汇编已知函数 ===')
for addr in [0x594390, 0x5195dc, 0x5168cc, 0x580350]:
    code = blob[addr:addr+32]
    insns = list(md.disasm(code, addr))
    print(f'  {addr:#x}: {len(insns)} 条指令')
    if insns:
        print(f'    第一条: {insns[0].mnemonic} {insns[0].op_str}')

# 从 0x594390 开始扫描 x1 写入（已知可反汇编区域）
print('\n=== 从 0x594390 扫描 x1 写入 ===')
count = 0
for insn in md.disasm(blob[0x594390:], 0x594390):
    if 'x1' in insn.op_str and any(m in insn.mnemonic for m in ['str', 'stp', 'stur']):
        print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}')
        count += 1
        if count >= 30:
            break
