import struct
import sys
from pathlib import Path
import re

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

# 搜索关键字符串
targets = [
    b'TAG_XL_TASK_INFO_EX',
    b'TAG_XL_TASK_INFO_EEX',
    b'TAG_TASK_PARAM_BT',
    b'TAG_TASK_PARAM_MAGNET',
    b'TAG_TASK_PARAM_EMULE',
    b'TAG_TORRENT_INFO',
    b'TAG_BT_SUBTASK_DETAIL',
    b'DownloadLib',
    b'xldownloadlib',
]

for t in targets:
    positions = []
    start = 0
    while True:
        pos = blob.find(t, start)
        if pos == -1:
            break
        positions.append(pos)
        start = pos + 1
    if positions:
        print(f'=== {t.decode()} ({len(positions)} 处) ===')
        for p in positions[:5]:
            print(f'  offset {p:#x}')
            # 打印周围上下文
            ctx = blob[max(0,p-20):p+len(t)+20]
            # 高亮目标字符串
            print(f'  上下文: {ctx[:20]} |{t}| {ctx[len(t)+20:len(t)+40]}')
        print()
