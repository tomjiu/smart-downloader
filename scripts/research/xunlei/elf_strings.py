import struct
from pathlib import Path
import re

SO = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_android\libxl_thunder_sdk.so')
blob = SO.read_bytes()

print(f'文件大小: {len(blob):,} bytes')

# 找所有可打印字符串（含 XL_ 前缀）
# 用正则找 ASCII 字符串，长度 >= 4
strings = re.findall(rb'[\x20-\x7e]{4,}', blob)
print(f'可打印字符串总数: {len(strings)}')

xl_strings = set()
for s in strings:
    t = s.decode('ascii', 'ignore')
    if 'XL_' in t or t.startswith('XL'):
        xl_strings.add(t)

print(f'\n=== 含 "XL" 的字符串（{len(xl_strings)} 个）===')
for s in sorted(xl_strings):
    print(f'  {s}')

# 也找下载引擎相关关键词
print(f'\n=== 下载引擎关键词 ===')
keywords = ['DownloadSDK', 'CreateTask', 'CreateBT', 'CreateMagnet', 'CreateP2sp',
            'StartTask', 'StopTask', 'DeleteTask', 'QueryTask', 'Thunder', 'thunder',
            'P2SP', 'p2sp', 'DHT', 'dht', 'emule', 'torrent']
for kw in keywords:
    hits = [s.decode('ascii','ignore') for s in strings if kw in s.decode('ascii','ignore')]
    if hits:
        print(f'  [{kw}] ({len(hits)} 个):')
        for h in hits[:10]:
            print(f'    {h}')
