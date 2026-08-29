import struct
from pathlib import Path

# 完整解析 .dynamic，直到真正结束（可能之前 size 读错）
SO = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_android\libxl_thunder_sdk.so')
blob = SO.read_bytes()

e_phoff = struct.unpack_from('<Q', blob, 32)[0]
e_phentsize = struct.unpack_from('<H', blob, 54)[0]
e_phnum = struct.unpack_from('<H', blob, 56)[0]

loads = []
for i in range(e_phnum):
    off = e_phoff + i * e_phentsize
    p_type = struct.unpack_from('<I', blob, off)[0]
    p_offset = struct.unpack_from('<Q', blob, off + 8)[0]
    p_vaddr = struct.unpack_from('<Q', blob, off + 16)[0]
    p_filesz = struct.unpack_from('<Q', blob, off + 32)[0]
    if p_type == 1:
        loads.append((p_vaddr, p_offset, p_filesz))
    if p_type == 2:
        dyn_off = p_offset
        dyn_size = p_filesz

def vaddr2off(va):
    for vaddr, off, filesz in loads:
        if vaddr <= va < vaddr + filesz:
            return off + (va - vaddr)
    return None

# 完整 dump dynamic
tag_names = {0:'NULL',1:'NEEDED',5:'STRTAB',6:'SYMTAB',10:'STRSZ',11:'SYMENT',
             0x6ffffef5:'GNU_HASH',0x6ffffff0:'VERSYM',0x6ffffffe:'VERNEED',0x6fffffff:'VERDEF'}
entries = []
i = 0
while i < dyn_size:
    d_tag = struct.unpack_from('<q', blob, dyn_off + i)[0]
    d_val = struct.unpack_from('<Q', blob, dyn_off + i + 8)[0]
    entries.append((d_tag, d_val))
    if d_tag == 0:
        break
    i += 16

print('=== 完整 .dynamic ===')
for tag, val in entries:
    print(f'  {tag_names.get(tag, hex(tag))} = {val:#x}')

# 用 GNU hash 或 STRTAB 找符号
strtab_vaddr = None
symtab_vaddr = None
for tag, val in entries:
    if tag == 5:
        strtab_vaddr = val
    if tag == 6:
        symtab_vaddr = val

print(f'\nstrtab vaddr={strtab_vaddr}, symtab vaddr={symtab_vaddr}')
