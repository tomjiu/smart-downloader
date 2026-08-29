import re
from pathlib import Path

SO = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_android\libxl_thunder_sdk.so')
blob = SO.read_bytes()

strings = re.findall(rb'[\x20-\x7e]{4,}', blob)

# 收集所有 XL 开头的字符串（这是函数名/日志标签）
xl_funcs = set()
xl_logs = set()
for s in strings:
    t = s.decode('ascii', 'ignore')
    # 纯函数名模式：XL[A-Za-z]+ （驼峰，无下划线）
    m = re.match(r'^(XL[A-Za-z0-9]+)$', t)
    if m:
        xl_funcs.add(m.group(1))
    # 日志标签模式：XLxxx + 参数
    m2 = re.match(r'^(XL[A-Za-z0-9]+)\s', t)
    if m2:
        xl_logs.add(m2.group(1))

all_xl = xl_funcs | xl_logs
print(f'=== 识别到的 XL 函数名（{len(all_xl)} 个，去重）===')
for s in sorted(all_xl):
    print(f'  {s}')

# 也找 _XL_ 下划线命名（Windows 风格）
print(f'\n=== 下划线命名 XL_ 函数（Windows 风格）===')
underscore = set()
for s in strings:
    t = s.decode('ascii', 'ignore')
    if re.match(r'^XL_[A-Za-z0-9_]+$', t):
        underscore.add(t)
for s in sorted(underscore):
    print(f'  {s}')
print(f'（共 {len(underscore)} 个）')
