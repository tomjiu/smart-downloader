import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)
md.detail = True

# 完整反汇编 _XLCreateBtTask，提取所有 [x20, #off] 字段访问
print('=== _XLCreateBtTask 完整反汇编（找 TAG_TASK_PARAM_BT 字段访问）===')
code = blob[0x594390:0x594390+0x220]
for insn in md.disasm(code, 0x594390):
    # 高亮 x20（param 指针）的字段访问
    mark = ''
    if 'x20' in insn.op_str and '#' in insn.op_str:
        mark = '  <<< param 字段访问'
    print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}{mark}')
