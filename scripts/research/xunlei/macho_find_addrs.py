import struct
import sys
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

BIN = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_macos\迅雷\Thunder.app\Contents\Bundles\XLEmbeddedPlayer.app\Contents\Frameworks\DownloadKit.framework\Versions\A\DownloadKit_arm64.bin')
blob = BIN.read_bytes()

symoff = 0x7ddb18
nsyms = 94892
stroff = 0x951be0

# 找关键符号地址
targets = [
    '_XLCreateBtTask',
    '_XLCreateBtMagnetTask',
    '_XLCreateP2spTask',
    '_XL_InitDownloadLib',
    '_XL_UnInitDownloadLib',
    '_XLGetTaskInfo',
    '_XLGetGlobalDownloadSpeed',
    '_XLStartTask',
    '_XLStopTask',
    '_XLReleaseTask',
    '__ZN11DownloadLib12CreateBtTaskEP17TAG_TASK_PARAM_BTPy',
    '__ZN11DownloadLib18CreateBtMagnetTaskEP21TAG_TASK_PARAM_MAGNETPy',
    '__ZN11DownloadLib14CreateP2spTaskEP14TAG_TASK_PARAMPy',
    '__ZN11DownloadLib11GetTaskInfoEyP19TAG_XL_TASK_INFO_EX',
]

found = {}
for i in range(nsyms):
    ent = symoff + i * 16
    n_strx = struct.unpack_from('<I', blob, ent)[0]
    n_type = struct.unpack_from('<B', blob, ent + 4)[0]
    n_sect = struct.unpack_from('<B', blob, ent + 5)[0]
    n_value = struct.unpack_from('<Q', blob, ent + 8)[0]
    if n_strx:
        end = blob.find(b'\x00', stroff + n_strx)
        sym = blob[stroff + n_strx:end].decode('ascii', 'ignore')
        if sym in targets:
            found[sym] = n_value
            print(f'  {sym} = {n_value:#x}')

print()
# 保存地址供后续反汇编用
import json
out = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_android\sym_addrs.json')
out.write_text(json.dumps({k: hex(v) for k, v in found.items()}, indent=2))
print(f'已保存 {len(found)} 个符号地址')
