import struct
import sys
import re
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

def decode_params(params):
    """尝试解码 Itanium mangled 参数"""
    result = []
    i = 0
    while i < len(params):
        c = params[i]
        if c == 'P':
            if i + 1 < len(params) and params[i+1] == 'K':
                result.append('const void*')
                i += 2
            elif i + 1 < len(params) and params[i+1] == 'c':
                result.append('const char*')
                i += 2
            elif i + 1 < len(params) and params[i+1] == 'h':
                result.append('const unsigned short*')
                i += 2
            elif i + 1 < len(params) and params[i+1] == 'y':
                result.append('const unsigned long long*')
                i += 2
            elif i + 1 < len(params) and params[i+1] == 'j':
                result.append('unsigned int*')
                i += 2
            elif i + 1 < len(params) and params[i+1] == 'i':
                result.append('int*')
                i += 2
            else:
                result.append('void*')
                i += 1
        elif c == 'K':
            result.append('const ')
            i += 1
        elif c == 'j':
            result.append('unsigned int')
            i += 1
        elif c == 'y':
            result.append('unsigned long long')
            i += 1
        elif c == 'i':
            result.append('int')
            i += 1
        elif c == 'b':
            result.append('bool')
            i += 1
        elif c == 'v':
            result.append('void')
            i += 1
        elif c == 'c':
            result.append('char')
            i += 1
        elif c == 'h':
            result.append('unsigned short')
            i += 1
        elif c == 'd':
            result.append('double')
            i += 1
        elif c == 'f':
            result.append('float')
            i += 1
        elif c == 'S':
            result.append('?')
            i += 1
        elif c == '_':
            result.append('_')
            i += 1
        else:
            result.append(c)
            i += 1
    return ', '.join(result)

pattern = re.compile(rb'_ZN11DownloadLib([A-Za-z0-9_]+)E([A-Za-z0-9_pvjwy]+)')

print('=== DownloadLib 函数签名（从 mangled 名提取）===')
print()

matches = []
for m in pattern.finditer(blob):
    start = m.start()
    full = m.group(0)
    func = m.group(1).decode('ascii', 'ignore')
    params = m.group(2).decode('ascii', 'ignore')
    if len(func) > 2 and len(params) > 1:
        matches.append((start, func, params, full))

seen = set()
unique = []
for start, func, params, full in matches:
    key = (func, params)
    if key not in seen:
        seen.add(key)
        unique.append((start, func, params, full))

print(f'找到 {len(unique)} 个不重复的 DownloadLib 函数签名\n')

unique.sort(key=lambda x: x[1])

for start, func, params, full in unique[:80]:
    param_str = decode_params(params)
    print(f'  DownloadLib::{func}({param_str})')

print(f'\n... 共 {len(unique)} 个函数')
