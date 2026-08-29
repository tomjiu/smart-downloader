import struct
import sys
import re
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

# 搜索所有 C++ mangled 名中包含 DownloadLib 的符号
# 格式通常是: __ZN11DownloadLib<FuncName>E<Params>
# 也搜索 TAG_TASK_PARAM_BT, TAG_XL_TASK_INFO_EX 等类型

targets = [
    b'DownloadLib',
    b'TAG_TASK_PARAM_BT',
    b'TAG_TASK_PARAM_MAGNET',
    b'TAG_XL_TASK_INFO_EX',
    b'TAG_XL_TASK_INFO_EEX',
    b'TAG_TASK_PARAM_EMULE',
    b'TAG_TORRENT_INFO',
    b'TAG_BT_SUBTASK_DETAIL',
]

# 找所有符号字符串的位置
print('=== 关键符号字符串出现位置 ===')
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
        print(f'\n{t.decode()} ({len(positions)} 处):')
        for p in positions[:10]:
            # 打印前后上下文（看看完整的 mangled 名）
            ctx_start = max(0, p - 20)
            ctx_end = min(len(blob), p + len(t) + 60)
            ctx = blob[ctx_start:ctx_end]
            # 尝试提取完整的符号名
            # C++ mangled 名以 __Z 或 _Z 开头
            # 也包含在字符串表中
            print(f'  0x{p:x}: ...{ctx.hex()}...')
