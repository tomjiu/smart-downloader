import struct
import sys
from pathlib import Path
from capstone import Cs, CS_ARCH_ARM64, CS_MODE_ARM

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

md = Cs(CS_ARCH_ARM64, CS_MODE_ARM)

# 检查 vtable 中的可疑值
for addr in [0x29bb64, 0x17fe30, 0x5044ac, 0x505cf0, 0x632dd0]:
    print(f'\n=== {addr:#x} ===')
    if addr >= len(blob):
        print('  超出文件范围')
        continue
    # 看它像代码还是数据
    code = blob[addr:addr+16]
    insns = list(md.disasm(code, addr))
    if insns:
        print(f'  反汇编: {insns[0].mnemonic} {insns[0].op_str}')
    else:
        # 可能是数据，打印 hex
        print(f'  数据: {code[:16].hex()}')
        # 看是否含可打印字符串
        if b'\x00' in code[:8]:
            end = code.find(b'\x00', 0, 8)
            if end > 0:
                try:
                    s = code[:end].decode('ascii', 'ignore')
                    if s.isprintable():
                        print(f'  字符串: "{s}"')
                except Exception:
                    pass

# 同时，让我重新理解 0x580350 的链表遍历
# 它读 [x0, #0x28] 作为链表头，然后 [node+0x10] 作为 task 对象
# 让我看 0x580350 的完整代码（不只是开头）
print('\n=== 0x580350 完整（链表遍历 + 找到后的路径）===')
code = blob[0x580350:0x580350+0x200]
for insn in md.disasm(code, 0x580350):
    print(f'  {insn.address:#x}: {insn.mnemonic:<10} {insn.op_str}')
