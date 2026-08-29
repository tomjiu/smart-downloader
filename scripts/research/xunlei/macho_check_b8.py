import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 检查 0x632dd0 处的实际内容
addr = 0x632dd0
print(f'=== {addr:#x} 附近 hex ===')
print(f'  {blob[addr:addr+32].hex()}')

# 尝试从不同偏移反汇编
for start in range(addr - 4, addr + 4):
    code = blob[start:start+32]
    insns = list(md.disasm(code, start))
    if insns:
        print(f'\n从 {start:#x} 反汇编:')
        for insn in insns[:6]:
            print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}')
        break
else:
    print('  无法反汇编（可能是数据）')

# 0x632dd0 的十六进制值
print(f'\n0x632dd0 的十六进制: {0x632dd0:#x} = {0x632dd0}')
# 作为小端 qword 存储在 vtable 中
print(f'vtable[0xb8] 存储的值: {struct.unpack_from("<Q", blob, 0x7258e8 + 0xb8)[0]:#x}')
