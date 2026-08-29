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

# 从 CreateBtTask 的 vtable 构造推导：
# str x8, [x0]  where x8 = 0x725000 + 0x8d8 + 0x10 = 0x7250e8
# 这是 BtTask 对象的 vtable
# GetTaskInfo 在链表节点上读 [vtable+0xb8] 作为填充函数
# 填充函数地址 = 0x7250e8 + 0xb8 = 0x7251a0

# 但等等，GetTaskInfo 路径的 vtable 是 0x724da8（0x50262c: str x8, [x0] where x8 = 0x724000+0xda8）
# 这是 GetTaskInfo 新分配对象的 vtable，不是链表节点的 vtable
# 链表节点是 CreateBtTask 创建的，vtable = 0x7250e8

# 让我先验证 0x7250e8 处的 vtable 内容
vtable_bt = 0x7250e8
print(f'=== BtTask vtable @ {vtable_bt:#x} ===')
# vtable 是函数指针数组，每个条目 8 字节（ARM64）
for i in range(30):
    addr = struct.unpack_from('<Q', blob, vtable_bt + i*8)[0]
    if addr == 0:
        continue
    # 验证地址是否在二进制内
    if addr < len(blob):
        # 反汇编该函数的前几条指令
        code = blob[addr:addr+16]
        insns = list(md.disasm(code, addr))
        if insns:
            first = insns[0]
            print(f'  [{i}] -> {addr:#x}: {first.mnemonic} {first.op_str}')

# 特别关注 +0xb8/8 = +23 项
print(f'\n=== vtable[0xb8/8] = vtable[23] ===')
addr = struct.unpack_from('<Q', blob, vtable_bt + 0xb8)[0]
print(f'  地址 = {addr:#x}')
if addr < len(blob):
    disasm(addr, 0x300, 'vtable[23] (BtTask::GetTaskInfoEx?)')
