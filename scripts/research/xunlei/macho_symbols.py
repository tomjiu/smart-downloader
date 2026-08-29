import struct
import sys
from pathlib import Path
from macholib.MachO import MachO

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')

# 使用 macholib 解析符号表
macho = MachO(str(BIN))

# 获取所有符号
symbols = []
for header in macho.headers:
    for cmd in header.commands:
        if hasattr(cmd, 'symtab') and cmd.symtab:
            symtab = cmd.symtab
            print(f'找到符号表: nlist={symtab.nlist}  str={symtab.strsize} bytes')
            
            with open(BIN, 'rb') as f:
                # 读取字符串表
                f.seek(symtab.stroff)
                strtab = f.read(symtab.strsize)
                
                # 读取 nlist 表
                f.seek(symtab.symtab)
                nlist_size = symtab.nlist * 16  # sizeof(struct nlist_64) = 16
                nlist_data = f.read(nlist_size)
                
                for i in range(symtab.nlist):
                    offset = i * 16
                    n_strx, n_type, n_sect, n_desc, n_value = struct.unpack_from('<IBBHQ', nlist_data, offset)
                    
                    # 读取符号名
                    if n_strx < len(strtab):
                        name_end = strtab.find(b'\x00', n_strx)
                        if name_end == -1:
                            name_end = len(strtab)
                        name = strtab[n_strx:name_end].decode('utf-8', 'ignore')
                        
                        # 只保留全局定义符号（N_EXT=1, N_TYPE=0x0e=N_SECT）
                        if (n_type & 0x0e) == 0x0e and (n_type & 0x01):  # N_EXT=1
                            symbols.append((n_value, name))

print(f'\n找到 {len(symbols)} 个外部符号')

# 过滤 _XL 前缀
xl_symbols = [(addr, name) for addr, name in symbols if name.startswith('_XL')]
print(f'\n=== _XL 前缀导出（共 {len(xl_symbols)} 个）===')
for addr, name in sorted(xl_symbols, key=lambda x: x[1]):
    print(f'  {addr:#10x}  {name}')
