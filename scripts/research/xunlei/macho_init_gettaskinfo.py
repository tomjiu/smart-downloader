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

# C 边界：_XLGetTaskInfo(task_id: u64, out: *mut TAG_XL_TASK_INFO_EX) -> i32
# 直接看 out 指针的字段写入
disasm(0x58f7f8, 0x200, '_XLGetTaskInfo (C 边界)')

# C 边界：_XL_InitDownloadLib(param: *const c_char) -> i32
# 看参数怎么解析
disasm(0x5f86ac, 0x200, '_XL_InitDownloadLib (C 边界)')
