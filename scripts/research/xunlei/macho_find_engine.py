import struct
import sys
import re
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

ROOT = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app')

# 下载引擎关键词
KEYWORDS = [
    b'XLCreateBtTask', b'XLCreateP2spTask', b'XLCreateEmuleTask',
    b'XLCreateBtMagnetTask', b'DownloadLib', b'XLStartTask', b'XLStopTask',
    b'XLGetTaskInfo', b'XLGetGlobalDownloadSpeed', b'p2sp', b'P2SP',
    b'xl_thunder_sdk', b'DownloadSDK', b'CreateTask', b'thunder_sdk',
    b'XLInit', b'XLUnInit', b'DHT', b'XLCreateMagnet',
]

# 遍历所有二进制文件（无扩展名或 .dylib/.framework 的可执行）
def is_binary(p):
    if p.is_dir():
        return False
    # 跳过资源文件
    ext = p.suffix.lower()
    if ext in ('.png', '.jpg', '.car', '.nib', '.plist', '.strings', '.css', '.ftl', '.icns', '.json', '.xml', '.txt', '.md'):
        return False
    return True

binaries = []
for p in ROOT.rglob('*'):
    try:
        if is_binary(p):
            # 检查是否真的是二进制（含大量非文本）
            try:
                head = p.read_bytes()[:4]
                if b'\x00' in head or head.startswith(b'\xcf\xfa') or head.startswith(b'\xca\xfe') or head.startswith(b'\xfe\xed'):
                    binaries.append(p)
            except Exception:
                pass
    except PermissionError:
        continue

print(f'找到 {len(binaries)} 个候选二进制文件\n')

for p in binaries:
    try:
        blob = p.read_bytes()
        if len(blob) < 1024:
            continue
        # 找下载引擎关键词
        hits = set()
        for kw in KEYWORDS:
            if kw in blob:
                hits.add(kw.decode('ascii', 'ignore'))
        if hits:
            rel = p.relative_to(ROOT)
            print(f'★ {rel}  ({len(blob)/1024/1024:.2f} MB)')
            print(f'   命中: {sorted(hits)}')
    except Exception as e:
        pass
