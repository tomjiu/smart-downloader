import struct
import sys
from pathlib import Path

sys.stdout.reconfigure(encoding='utf-8', errors='replace')

DMG = Path(r'C:\Users\yezi6\Downloads\thunder_5.80.7.66659.dmg')
blob = DMG.read_bytes()

koly = blob[-512:]
xml_offset = struct.unpack_from('>Q', koly, 0xd8)[0]
xml_length = struct.unpack_from('>Q', koly, 0xe0)[0]

xml_blob = blob[xml_offset:xml_offset+xml_length]
start = xml_blob.find(b'<?xml')
xml = xml_blob[start:].decode('utf-8', 'ignore')

# 写完整 plist 到文件
out = Path(r'E:\Code\ai\smart-downloader\scripts\research\xunlei\extracted_android\dmg_plist.xml')
out.write_text(xml, encoding='utf-8')
print(f'完整 plist 已写 {out} ({len(xml)} 字符)')

# 提取 partition 信息：name + 压缩类型 + 数据偏移
import re
# 找 block 结构（每个 partition 是一个 dict）
# 关键 key: Name, Attributes, Data (blkx)
print('\n=== plist 关键内容 ===')
print(xml[:5000])
