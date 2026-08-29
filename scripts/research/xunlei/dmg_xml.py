import struct
from pathlib import Path

DMG = Path(r'C:\Users\yezi6\Downloads\thunder_5.80.7.66659.dmg')
blob = DMG.read_bytes()

koly = blob[-512:]
xml_offset = struct.unpack_from('>Q', koly, 0xd8)[0]
xml_length = struct.unpack_from('>Q', koly, 0xe0)[0]

xml_blob = blob[xml_offset:xml_offset+xml_length]
start = xml_blob.find(b'<?xml')
if start == -1:
    start = xml_blob.find(b'<plist')
xml = xml_blob[start:].decode('utf-8', 'ignore')

# 提取关键信息：块名、压缩类型
import re
# 找 name 和 blockType
names = re.findall(r'<key>Name</key>\s*<string>([^<]+)</string>', xml)
print('=== 分区名 ===')
for n in names:
    print(f'  {n}')

print('\n=== 完整 XML plist（前 6000 字符）===')
print(xml[:6000])
