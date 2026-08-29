import struct
from pathlib import Path

SO = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_android\libxl_thunder_sdk.so')
blob = SO.read_bytes()

# 解析 ELF64 头
assert blob[:4] == b'\x7fELF'
e_type = struct.unpack_from('<H', blob, 16)[0]
e_machine = struct.unpack_from('<H', blob, 18)[0]
e_shoff = struct.unpack_from('<Q', blob, 40)[0]
e_shentsize = struct.unpack_from('<H', blob, 58)[0]
e_shnum = struct.unpack_from('<H', blob, 60)[0]
e_shstrndx = struct.unpack_from('<H', blob, 62)[0]

print(f'ELF64, type={e_type}, machine={hex(e_machine)}')
print(f'section headers: {e_shnum} 个')

# 读 section header 表
def read_sh(i):
    off = e_shoff + i * e_shentsize
    sh_name = struct.unpack_from('<I', blob, off)[0]
    sh_type = struct.unpack_from('<I', blob, off + 4)[0]
    sh_offset = struct.unpack_from('<Q', blob, off + 24)[0]
    sh_size = struct.unpack_from('<Q', blob, off + 32)[0]
    sh_link = struct.unpack_from('<I', blob, off + 40)[0]
    sh_entsize = struct.unpack_from('<Q', blob, off + 56)[0]
    return sh_name, sh_type, sh_offset, sh_size, sh_link, sh_entsize

# 找 .dynsym（动态符号）和 .dynstr
dynsym = None
dynstr = None
shstr = read_sh(e_shstrndx)
shstr_data = blob[shstr[2]:shstr[2]+shstr[3]]

sections = []
for i in range(e_shnum):
    name_off, stype, soff, ssize, slink, sentsize = read_sh(i)
    # 读节名
    end = shstr_data.find(b'\x00', name_off)
    name = shstr_data[name_off:end].decode('ascii', 'ignore')
    sections.append((name, stype, soff, ssize, slink, sentsize))
    if name == '.dynsym':
        dynsym = (soff, ssize, slink, sentsize)
    if name == '.dynstr':
        dynstr = (soff, ssize)

print(f'\n找到 .dynsym: {dynsym is not None}')
print(f'找到 .dynstr: {dynstr is not None}')

# 解析动态符号，找导出符号（st_shndx != 0 且 bind 为 global/weak）
if dynsym and dynstr:
    soff, ssize, strlink, sentsize = dynsym
    stroff, strsize = dynstr
    n_syms = ssize // sentsize
    exports = []
    for i in range(n_syms):
        ent = soff + i * sentsize
        st_name = struct.unpack_from('<I', blob, ent)[0]
        st_info = struct.unpack_from('<B', blob, ent + 4)[0]
        st_shndx = struct.unpack_from('<H', blob, ent + 6)[0]
        if st_name != 0 and st_shndx != 0:
            bind = st_info >> 4
            if bind in (1, 2):  # GLOBAL or WEAK
                end = blob.find(b'\x00', stroff + st_name)
                sym = blob[stroff + st_name:end].decode('ascii', 'ignore')
                exports.append(sym)
    exports.sort()
    print(f'\n=== 导出符号总数: {len(exports)} ===')
    # 找 XL_ 前缀
    xl_syms = [s for s in exports if s.startswith('XL_')]
    print(f'=== XL_ 前缀符号: {len(xl_syms)} 个 ===')
    for s in xl_syms:
        print(f'  {s}')
