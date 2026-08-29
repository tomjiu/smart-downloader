import struct
import sys
import re
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 搜索所有以 x1 为目标的内存写入指令（很可能是填充 out 结构体）
# 过滤：只关注 x1 + 小偏移（0-0x200 内），且不在已知调度函数内
print('=== 搜索对 x1 指针的字段写入（可能填充 TAG_XL_TASK_INFO_EX）===')
print('（仅列出偏移 0x00..0x200 内的 str/stp 到 [x1, #imm]）')

# 先反汇编一批候选函数，找写 [x1, #off] 的
candidates = [
    0x580350, 0x580398, 0x5804d4,  # GetTaskInfo 调用链
    0x502614, 0x502638, 0x5026b4,  # 调度函数
    0x5168cc, 0x5169b8,            # DownloadLib::GetTaskInfo
]

# 也搜索包含 'TAG_XL_TASK_INFO_EX' 字符串附近的函数
# 字符串在 0x7693a0，附近可能有类型信息或函数指针
tag_str = 0x7693a0
print(f'\n=== TAG_XL_TASK_INFO_EX 字符串附近（0x{tag_str:#x}）===')
code = blob[tag_str-0x40:tag_str+0x40]
for insn in md.disasm(code, tag_str-0x40):
    print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}')
