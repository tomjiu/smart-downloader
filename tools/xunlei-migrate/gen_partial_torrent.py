import sys
import hashlib

piece_length = 16384
num_pieces = 16
file_size = 256 * 1024

pieces_hash = []
for p in range(num_pieces):
    buf = bytes([p % 256] * piece_length)
    h = hashlib.sha1(buf).hexdigest()
    pieces_hash.append(h)

# 构造 info dict
info = bytearray()
info += b"d6:lengthi"
info += str(file_size).encode()
info += b"e4:name12:partial_test"  # 修正：partial_test 是 12 字节
info += b"12:piece lengthi"
info += str(piece_length).encode()
info += b"e6:pieces"
info += str(num_pieces * 20).encode()
info += b":"
for h in pieces_hash:
    info += bytes.fromhex(h)
info += b"e"

# 顶层 dict
torrent = bytearray()
torrent += b"d8:announce14:http://tracker"
torrent += b"4:info"
torrent += bytes(info)
torrent += b"e"

print(f"torrent length: {len(torrent)}")

info_hash = hashlib.sha1(bytes(info)).hexdigest()
print(f"info_hash: {info_hash}")

# 写文件
with open("tools/xunlei-migrate/e2e_out/partial_test.torrent", "wb") as f:
    f.write(torrent)
print("written to partial_test.torrent")
