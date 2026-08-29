import struct
from pathlib import Path

SO = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_android\libxl_thunder_sdk.so')
blob = SO.read_bytes()

assert blob[:4] == b'\x7fELF'
e_phoff = struct.unpack_from('<Q', blob, 32)[0]
e_phentsize = struct.unpack_from('<H', blob, 54)[0]
e_phnum = struct.unpack_from('<H', blob, 56)[0]

print(f'program headers: {e_phnum} 个 @ offset {e_phoff}, entsize {e_phentsize}')

# 遍历 program headers，找 PT_DYNAMIC (type=2)
dynamic_off = None
dynamic_size = None
for i in range(e_phnum):
    off = e_phoff + i * e_phentsize
    p_type = struct.unpack_from('<I', blob, off)[0]
    p_offset = struct.unpack_from('<Q', blob, off + 8)[0]
    p_filesz = struct.unpack_from('<Q', blob, off + 32)[0]
    if p_type == 2:  # PT_DYNAMIC
        dynamic_off = p_offset
        dynamic_size = p_filesz
        print(f'PT_DYNAMIC @ file offset {p_offset:#x}, size {p_filesz}')

if dynamic_off is None:
    print('未找到 PT_DYNAMIC')
    exit()

# 解析 .dynamic 段
DT_NULL = 0
DT_SYMTAB = 6
DT_STRTAB = 5
DT_STRSZ = 10
DT_SYMENT = 11

symtab_vaddr = None
strtab_vaddr = None
strsz = None
syment = None

i = 0
while i < dynamic_size:
    d_tag = struct.unpack_from('<q', blob, dynamic_off + i)[0]
    d_val = struct.unpack_from('<Q', blob, dynamic_off + i + 8)[0]
    if d_tag == DT_SYMTAB:
        symtab_vaddr = d_val
    elif d_tag == DT_STRTAB:
        strtab_vaddr = d_val
    elif d_tag == DT_STRSZ:
        strsz = d_val
    elif d_tag == DT_SYMENT:
        syment = d_val
    elif d_tag == DT_NULL:
        break
    i += 16

print(f'symtab vaddr={symtab_vaddr:#x}, strtab vaddr={strtab_vaddr:#x}, strsz={strsz}, syment={syment}')

# vaddr -> file offset 需要 program header 的 LOAD 段映射
loads = []
for i in range(e_phnum):
    off = e_phoff + i * e_phentsize
    p_type = struct.unpack_from('<I', blob, off)[0]
    if p_type == 1:  # PT_LOAD
        p_offset = struct.unpack_from('<Q', blob, off + 8)[0]
        p_vaddr = struct.unpack_from('<Q', blob, off + 16)[0]
        p_filesz = struct.unpack_from('<Q', blob, off + 32)[0]
        loads.append((p_vaddr, p_offset, p_filesz))

def vaddr2off(va):
    for vaddr, off, filesz in loads:
        if vaddr <= va < vaddr + filesz:
            return off + (va - vaddr)
    return None

# 解析符号表，找 XL_ 导出
symoff = vaddr2off(symtab_vaddr)
stroff = vaddr2off(strtab_vaddr)
print(f'symtab file off={symoff:#x}, strtab file off={stroff:#x}')

if symoff is None or stroff is None:
    print('无法映射 vaddr')
    exit()

exports = []
# 符号表大小未知，用 strsz 和 syment 估算（实际符号数需要遍历直到越界）
# 先假设符号表在 strtab 之前，大小 = strtab_vaddr - symtab_vaddr
symtab_size = strtab_vaddr - symtab_vaddr
n_syms = symtab_size // syment
print(f'估算符号数: {n_syms}')

for i in range(n_syms):
    ent = symoff + i * syment
    st_name = struct.unpack_from('<I', blob, ent)[0]
    st_info = struct.unpack_from('<B', blob, ent + 4)[0]
    st_shndx = struct.unpack_from('<H', blob, ent + 6)[0]
    if st_name != 0 and st_shndx != 0:
        bind = st_info >> 4
        if bind in (1, 2):
            end = blob.find(b'\x00', stroff + st_name)
            sym = blob[stroff + st_name:end].decode('ascii', 'ignore')
            exports.append(sym)

exports.sort()
print(f'\n导出符号总数: {len(exports)}')
xl = [s for s in exports if s.startswith('XL_')]
print(f'XL_ 前缀符号: {len(xl)} 个')
for s in xl:
    print(f'  {s}')
