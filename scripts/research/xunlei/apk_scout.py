import zipfile
from pathlib import Path

APK = Path(r'C:\Users\yezi6\Downloads\x-player-guanwang.apk')

z = zipfile.ZipFile(APK)
names = z.namelist()

print(f'APK 总条目数: {len(names)}')
print()

# 1. 找所有 .so 原生库
so_files = [n for n in names if n.endswith('.so')]
print(f'=== .so 原生库（{len(so_files)} 个）===')
for n in so_files:
    info = z.getinfo(n)
    print(f'  {n}  ({info.file_size:,} bytes)')

print()

# 2. 找所有 .dex（Java 字节码）
dex = [n for n in names if n.endswith('.dex')]
print(f'=== .dex（{len(dex)} 个）===')
for n in dex:
    info = z.getinfo(n)
    print(f'  {n}  ({info.file_size:,} bytes)')

print()

# 3. 顶层目录结构
top = set()
for n in names:
    parts = n.split('/')
    if parts[0] == 'lib':
        top.add('/'.join(parts[:2]) if len(parts) > 1 else parts[0])
    else:
        top.add(parts[0])
print('=== 顶层结构 ===')
for t in sorted(top):
    print(f'  {t}')

# 4. 找跟下载引擎相关的关键词
print()
print('=== 含下载引擎关键词的条目 ===')
keywords = ['thunder', 'download', 'xunlei', 'p2p', 'p2sp', 'dht', 'bt', 'emule', 'torrent']
seen = set()
for n in names:
    lower = n.lower()
    for kw in keywords:
        if kw in lower:
            base = n.split('/')[-1]
            if base not in seen:
                seen.add(base)
                print(f'  {n}')
            break
