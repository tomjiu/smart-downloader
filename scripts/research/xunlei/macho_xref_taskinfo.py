import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 搜索整个二进制中引用 0x7693a0 (TAG_XL_TASK_INFO_EX 字符串) 的代码地址
# 方法：找 adrp/add 对，其目标地址在 0x7693a0 附近
target = 0x7693a0

print('=== 搜索引用 TAG_XL_TASK_INFO_EX 字符串的代码 ===')
# 遍历所有指令，找 adrp x*, #0x769000 附近 + add x*, x*, #0xa0 附近
for insn in md.disasm(blob, 0):
    if insn.mnemonic == 'adrp' and 'x' in insn.op_str:
        # 解析 adrp 目标
        m = re.match(r'adrp\s+(x\d+),\s+#0x([0-9a-fA-F]+)', insn.op_str)
        if m:
            reg = m.group(1)
            val = int(m.group(2), 16)
            # 检查是否接近 target
            if abs(val - (target & ~0xfff)) < 0x5000:
                # 看接下来的 add 指令
                next_off = insn.address + 4
                if next_off + 8 < len(blob):
                    next_insns = list(md.disasm(blob[next_off:next_off+8], next_off))
                    if next_insns and next_insns[0].mnemonic == 'add':
                        add_str = next_insns[0].op_str
                        if reg in add_str:
                            print(f'  {insn.address:#x}: {insn.mnemonic} {insn.op_str}')
                            print(f'    {next_insns[0].address:#x}: {next_insns[0].mnemonic} {add_str}')

import re
