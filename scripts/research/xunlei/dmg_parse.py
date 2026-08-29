import struct
from pathlib import Path

DMG = Path(r'C:\Users\yezi6\Downloads\thunder_5.80.7.66659.dmg')
blob = DMG.read_bytes()
size = len(blob)
print(f'DMG 大小: {size:,} bytes ({size/1024/1024:.1f} MB)')

# DMG 的 koly trailer 在最后 512 字节
koly = blob[-512:]
print(f'\n=== koly trailer（最后 512 字节）===')
print(f'  signature: {koly[:4]}')

# UDIF 结构：koly 里有 XML plist 偏移
# koly 结构关键字段（从末尾往前）：
#   +0x000: signature "koly" (4)
#   +0x004: version (4)
#   +0x008: headerSize (4)
#   +0x00c: flags (4)
#   +0x010: runningDataForkOffset (8)
#   +0x018: dataForkOffset (8)
#   +0x020: dataForkLength (8)
#   +0x028: rsrcForkOffset (8)
#   +0x030: rsrcForkLength (8)
#   +0x038: segmentNumber (4)
#   +0x03c: segmentCount (4)
#   +0x040: segmentID (16)
#   +0x050: dataChecksum type (4)
#   +0x054: dataChecksum size (4)
#   +0x058: dataChecksum (128)
#   +0x0d8: xmlOffset (8)
#   +0x0e0: xmlLength (8)
#   +0x1f0: masterChecksum (128)
#   +0x270: imageVariant (4)
#   +0x274: sectorCount (8)
#   +0x27c: ...

sig = koly[:4]
print(f'  signature 字节: {sig}')

xml_offset = struct.unpack_from('>Q', koly, 0xd8)[0]
xml_length = struct.unpack_from('>Q', koly, 0xe0)[0]
print(f'  xmlOffset={xml_offset}, xmlLength={xml_length}')

sector_count = struct.unpack_from('>Q', koly, 0x274)[0]
print(f'  sectorCount={sector_count}')

# 读 XML plist（描述块）
if xml_offset and xml_length:
    xml_blob = blob[xml_offset:xml_offset+xml_length]
    print(f'\n=== XML plist（前 2000 字符）===')
    # 找第一个 < 开始
    start = xml_blob.find(b'<?xml')
    if start == -1:
        start = xml_blob.find(b'<plist')
    print(xml_blob[start:start+2000].decode('utf-8', 'ignore'))
