import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM
import re

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 搜索所有写 [x1, #0x00..0x100] 的指令
# 这些很可能就是填充 out 结构体的函数
print('=== 写 [x1, #0x00..0x100] 的指令（按函数分组）===')

current_func = None
func_writes = []

for insn in md.disasm(blob, 0):
    if insn.mnemonic in ('b', 'br', 'ret', 'cbz', 'cbnz', 'tbz', 'tbnz'):
        if func_writes:
            # 只显示包含多个写入的函数（填充函数会写多个字段）
            if len(func_writes) >= 2:
                print(f'\n@ {current_func:#x} ({len(func_writes)} 次写入):')
                for a, m, o, imm in func_writes:
                    print(f'  {a:#x}: {m:<8} {o}  (imm={imm:#x})')
            func_writes = []
        # 更新当前函数（简单：用 ret/b 后的地址）
        current_func = insn.address + 4
    
    if insn.mnemonic in ('str', 'stp', 'strb', 'strh', 'stur', 'sturq', 'sturw'):
        # 匹配 [x1, #imm] 其中 imm 是无符号立即数
        m = re.search(r'\[x1,\s*#(0x[0-9a-fA-F]+|\d+)\]', insn.op_str)
        if m:
            imm_str = m.group(1)
            if imm_str.startswith('0x'):
                imm = int(imm_str, 16)
            else:
                imm = int(imm_str)
            if 0 <= imm <= 0x100:
                func_writes.append((insn.address, insn.mnemonic, insn.op_str, imm))

# 打印最后一个函数
if func_writes and len(func_writes) >= 2:
    print(f'\n@ {current_func:#x} ({len(func_writes)} 次写入):')
    for a, m, o, imm in func_writes:
        print(f'  {a:#x}: {m:<8} {o}  (imm={imm:#x})')
