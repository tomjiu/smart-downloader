import struct
from pathlib import Path

SO = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_android\libxl_thunder_sdk.so')
blob = SO.read_bytes()

e_phoff = struct.unpack_from('<Q', blob, 32)[0]
e_phentsize = struct.unpack_from('<H', blob, 54)[0]
e_phnum = struct.unpack_from('<H', blob, 56)[0]

loads = []
dynamic_off = None
dynamic_size = None
for i in range(e_phnum):
    off = e_phoff + i * e_phentsize
    p_type = struct.unpack_from('<I', blob, off)[0]
    p_offset = struct.unpack_from('<Q', blob, off + 8)[0]
    p_vaddr = struct.unpack_from('<Q', blob, off + 16)[0]
    p_filesz = struct.unpack_from('<Q', blob, off + 32)[0]
    p_flags = struct.unpack_from('<I', blob, off + 4)[0]
    print(f'PH[{i}] type={p_type} flags={p_flags:#x} offset={p_offset:#x} vaddr={p_vaddr:#x} filesz={p_filesz:#x}')
    if p_type == 1:
        loads.append((p_vaddr, p_offset, p_filesz))
    if p_type == 2:
        dynamic_off = p_offset
        dynamic_size = p_filesz

print(f'\n.dynamic @ {dynamic_off:#x}, size {dynamic_size}')

# dump 所有 dynamic 条目
tag_names = {
    0:'NULL', 1:'NEEDED', 2:'PLTRELSZ', 3:'PLTGOT', 4:'HASH', 5:'STRTAB', 6:'SYMTAB',
    7:'RELA', 8:'RELASZ', 9:'RELAENT', 10:'STRSZ', 11:'SYMENT', 12:'INIT', 13:'FINI',
    14:'SONAME', 15:'RPATH', 16:'SYMBOLIC', 17:'REL', 18:'RELSZ', 19:'RELENT',
    20:'PLTREL', 21:'DEBUG', 22:'TEXTREL', 23:'JMPREL', 24:'BIND_NOW', 25:'INIT_ARRAY',
    26:'FINI_ARRAY', 27:'INIT_ARRAYSZ', 28:'FINI_ARRAYSZ', 0x6ffffff0:'VERSYM',
    0x6ffffff9:'RELACOUNT', 0x6ffffef5:'GNU_HASH', 0x6ffffffe:'VERNEED', 0x6fffffff:'VERSYM',
}

i = 0
while i < dynamic_size:
    d_tag = struct.unpack_from('<q', blob, dynamic_off + i)[0]
    d_val = struct.unpack_from('<Q', blob, dynamic_off + i + 8)[0]
    name = tag_names.get(d_tag, f'0x{d_tag:x}')
    print(f'  tag={d_tag} ({name}) val={d_val:#x}')
    if d_tag == 0:
        break
    i += 16
