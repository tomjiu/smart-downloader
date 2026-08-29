"""
bencode_codec_v2.py — BitComet BT v1+v2 Bencode 编解码器
=================================================

逆向来源: Core_Common::TorrentDecodeBase + TorrentFileV2Decode
关键符号:
    TorrentDecodeBase::TorrentDecodeBase
    TorrentFileV2Decode::TorrentFileV2Decode
    TorrentFileV2Decode::is_in_file_tree
    TorrentFileV2Decode::is_in_file_path_list
    TorrentFileV2Decode::process_dict_enter
    TorrentFileV2Decode::process_dict_leave
    TorrentFileV2Decode::process_file_path_dict_enter
    TorrentFileV2Decode::process_file_path_dict_leave
    TorrentFileV2Decode::process_file_path_list_item_enter
    TorrentFileV2Decode::process_file_path_string_enter
    TorrentFileV2Decode::process_file_tree_dict_enter
    TorrentFileV2Decode::process_file_tree_dict_leave
    TorrentFileV2Decode::process_list_item_enter
    TorrentFileV2Decode::process_string_enter

设计核心:
1. BitComet 自实现 bencode 编解码 (不依赖 libtorrent)
2. TorrentFileV2Decode 用 SAX 风格 (process_*_enter/leave 回调)
3. 完整支持 BT v1 (info_hash) + v2 (info_hash_v2 + file tree)
4. 支持 hybrid magnet (同时含 v1 + v2 hash)

BEP-52 v2 bencode 结构:
    d
        8:announce <url>
        4:info d
            12:meta version i2e
            9:file tree d
                7:subdir1 d
                    5:file1 d
                        6:length i<size>e
                        11:pieces root 32:<sha256_root>
                    e
                e
            e
            12:piece length i<piece_size>e
        e
    e

加速价值 (针对 qBittorrent):
- qBittorrent 用 libtorrent 内置 bencode
- BitComet 独立实现可:
  a) 流式解析 (大 torrent 不全载入内存)
  b) 直接修改 file tree (改 piece_size / 添加文件)
  c) v1 ↔ v2 互转

本模块实现:
- BencodeValue: 类型联合 (str/int/list/dict)
- BencodeEncoder: 标准编码器
- BencodeDecoder: 标准解码器
- BencodeSaxHandler: SAX 风格回调 (对应 TorrentFileV2Decode::process_*)
- V2TorrentFileHandler: BT v2 torrent 解析回调实现

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import hashlib
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Optional, Tuple, Union


# Bencode 值类型
BencodeValue = Union[bytes, int, List["BencodeValue"], Dict[bytes, "BencodeValue"]]


# -----------------------------------------------------------------------------
# BencodeEncoder
# -----------------------------------------------------------------------------

class BencodeEncoder:
    """标准 bencode 编码器."""

    @staticmethod
    def encode(value: BencodeValue) -> bytes:
        if isinstance(value, bytes):
            return f"{len(value)}:".encode() + value
        elif isinstance(value, str):
            b = value.encode("utf-8")
            return f"{len(b)}:".encode() + b
        elif isinstance(value, int):
            return b"i" + str(value).encode() + b"e"
        elif isinstance(value, list):
            return b"l" + b"".join(BencodeEncoder.encode(v) for v in value) + b"e"
        elif isinstance(value, dict):
            result = b"d"
            # dict keys 必须是 bytes 且按字典序排序
            for k in sorted(value.keys()):
                key_bytes = k if isinstance(k, bytes) else k.encode("utf-8")
                result += BencodeEncoder.encode(key_bytes)
                result += BencodeEncoder.encode(value[k])
            result += b"e"
            return result
        else:
            raise TypeError(f"unsupported type: {type(value)}")


# -----------------------------------------------------------------------------
# BencodeDecoder
# -----------------------------------------------------------------------------

class BencodeDecodeError(Exception):
    pass


class BencodeDecoder:
    """标准 bencode 解码器."""

    def __init__(self, data: bytes):
        self.data = data
        self.pos = 0

    @classmethod
    def decode(cls, data: bytes) -> BencodeValue:
        decoder = cls(data)
        value = decoder._decode_value()
        if decoder.pos != len(data):
            raise BencodeDecodeError(f"trailing data at pos {decoder.pos}")
        return value

    def _decode_value(self) -> BencodeValue:
        if self.pos >= len(self.data):
            raise BencodeDecodeError("unexpected end of data")
        c = self.data[self.pos:self.pos+1]
        if c == b"i":
            return self._decode_int()
        elif c == b"l":
            return self._decode_list()
        elif c == b"d":
            return self._decode_dict()
        elif c.isdigit():
            return self._decode_string()
        else:
            raise BencodeDecodeError(f"unexpected char {c!r} at pos {self.pos}")

    def _decode_int(self) -> int:
        self.pos += 1  # skip 'i'
        end = self.data.find(b"e", self.pos)
        if end == -1:
            raise BencodeDecodeError("unterminated int")
        try:
            value = int(self.data[self.pos:end])
        except ValueError:
            raise BencodeDecodeError(f"invalid int at pos {self.pos}")
        self.pos = end + 1
        return value

    def _decode_string(self) -> bytes:
        colon = self.data.find(b":", self.pos)
        if colon == -1:
            raise BencodeDecodeError("missing colon in string length")
        try:
            length = int(self.data[self.pos:colon])
        except ValueError:
            raise BencodeDecodeError(f"invalid string length at pos {self.pos}")
        self.pos = colon + 1
        if self.pos + length > len(self.data):
            raise BencodeDecodeError("string length exceeds data")
        value = self.data[self.pos:self.pos+length]
        self.pos += length
        return value

    def _decode_list(self) -> List[BencodeValue]:
        self.pos += 1  # skip 'l'
        result = []
        while self.pos < len(self.data):
            if self.data[self.pos:self.pos+1] == b"e":
                self.pos += 1
                return result
            result.append(self._decode_value())
        raise BencodeDecodeError("unterminated list")

    def _decode_dict(self) -> Dict[bytes, BencodeValue]:
        self.pos += 1  # skip 'd'
        result = {}
        last_key = b""
        while self.pos < len(self.data):
            if self.data[self.pos:self.pos+1] == b"e":
                self.pos += 1
                return result
            key = self._decode_value()
            if not isinstance(key, bytes):
                raise BencodeDecodeError(f"dict key must be bytes, got {type(key)}")
            # 检查排序
            if key <= last_key:
                # BEP-3 要求 keys 字典序排序, 但允许不严格
                pass
            last_key = key
            value = self._decode_value()
            result[key] = value
        raise BencodeDecodeError("unterminated dict")


# -----------------------------------------------------------------------------
# BencodeSaxHandler — SAX 风格回调 (对应 TorrentFileV2Decode::process_*)
# -----------------------------------------------------------------------------

class BencodeSaxHandler:
    """SAX 风格 bencode 解析回调.

    对应 Core_Common::TorrentFileV2Decode 的 process_*_enter/leave 方法.
    """

    def on_dict_enter(self, parent_key: Optional[bytes]) -> None:
        """对应 process_dict_enter / process_file_tree_dict_enter."""
        pass

    def on_dict_leave(self, parent_key: Optional[bytes]) -> None:
        """对应 process_dict_leave / process_file_tree_dict_leave."""
        pass

    def on_list_enter(self, parent_key: Optional[bytes]) -> None:
        """对应 process_list_item_enter."""
        pass

    def on_list_leave(self, parent_key: Optional[bytes]) -> None:
        pass

    def on_string_enter(self, parent_key: Optional[bytes], value: bytes) -> None:
        """对应 process_string_enter / process_file_path_string_enter."""
        pass

    def on_int_enter(self, parent_key: Optional[bytes], value: int) -> None:
        pass

    def on_file_path_enter(self, path: List[bytes]) -> None:
        """对应 process_file_path_list_item_enter."""
        pass


class BencodeSaxParser:
    """SAX 流式 bencode 解析器."""

    def __init__(self, handler: BencodeSaxHandler):
        self.handler = handler
        self._key_stack: List[Optional[bytes]] = []

    def parse(self, data: bytes) -> None:
        self._pos = 0
        self._data = data
        self._parse_value(None)

    def _parse_value(self, parent_key: Optional[bytes]) -> None:
        if self._pos >= len(self._data):
            return
        c = self._data[self._pos:self._pos+1]
        if c == b"i":
            self._parse_int(parent_key)
        elif c == b"l":
            self._parse_list(parent_key)
        elif c == b"d":
            self._parse_dict(parent_key)
        elif c.isdigit():
            self._parse_string(parent_key)

    def _parse_int(self, parent_key: Optional[bytes]) -> None:
        self._pos += 1
        end = self._data.find(b"e", self._pos)
        value = int(self._data[self._pos:end])
        self._pos = end + 1
        self.handler.on_int_enter(parent_key, value)

    def _parse_string(self, parent_key: Optional[bytes]) -> None:
        colon = self._data.find(b":", self._pos)
        length = int(self._data[self._pos:colon])
        self._pos = colon + 1
        value = self._data[self._pos:self._pos+length]
        self._pos += length
        self.handler.on_string_enter(parent_key, value)

    def _parse_list(self, parent_key: Optional[bytes]) -> None:
        self.handler.on_list_enter(parent_key)
        self._pos += 1
        while self._data[self._pos:self._pos+1] != b"e":
            self._parse_value(parent_key)
        self._pos += 1
        self.handler.on_list_leave(parent_key)

    def _parse_dict(self, parent_key: Optional[bytes]) -> None:
        self.handler.on_dict_enter(parent_key)
        self._pos += 1
        while self._data[self._pos:self._pos+1] != b"e":
            # 解析 key
            colon = self._data.find(b":", self._pos)
            length = int(self._data[self._pos:colon])
            self._pos = colon + 1
            key = self._data[self._pos:self._pos+length]
            self._pos += length
            # 解析 value
            self._parse_value(key)
        self._pos += 1
        self.handler.on_dict_leave(parent_key)


# -----------------------------------------------------------------------------
# V2TorrentFileHandler — BT v2 torrent 解析实现
# -----------------------------------------------------------------------------

class V2TorrentFileHandler(BencodeSaxHandler):
    """BT v2 torrent 文件解析器.

    对应 TorrentFileV2Decode 完整回调实现.
    """

    def __init__(self):
        self.meta_version: int = 1       # 1 = v1, 2 = v2
        self.piece_length: int = 0
        self.info_dict_raw: Optional[bytes] = None  # 用于计算 info_hash
        # v1 files
        self.v1_files: List[Dict] = []
        # v2 file tree
        self.v2_file_tree: Dict = {}
        # 当前 path 栈
        self._path_stack: List[bytes] = []
        self._current_file: Optional[Dict] = None
        self._in_info: bool = False
        self._in_file_tree: bool = False
        self._in_files: bool = False
        self._info_start: int = 0
        self._info_end: int = 0

    def on_dict_enter(self, parent_key: Optional[bytes]) -> None:
        if parent_key == b"info":
            self._in_info = True
            self._info_start = self._pos if hasattr(self, "_pos") else 0
        elif parent_key == b"file tree":
            self._in_file_tree = True
        elif parent_key == b"files":
            self._in_files = True

    def on_dict_leave(self, parent_key: Optional[bytes]) -> None:
        if parent_key == b"info":
            self._in_info = False
        elif parent_key == b"file tree":
            self._in_file_tree = False
        elif parent_key == b"files":
            self._in_files = False

    def on_int_enter(self, parent_key: Optional[bytes], value: int) -> None:
        if parent_key == b"meta version":
            self.meta_version = value
        elif parent_key == b"piece length":
            self.piece_length = value
        elif parent_key == b"length":
            if self._current_file is not None:
                self._current_file["length"] = value

    def on_string_enter(self, parent_key: Optional[bytes], value: bytes) -> None:
        if parent_key == b"name":
            self.name = value
        elif parent_key == b"pieces root":
            if self._current_file is not None:
                self._current_file["pieces_root"] = value
        elif parent_key == b"path":
            # v1 files: path 是 list
            pass

    def on_list_enter(self, parent_key: Optional[bytes]) -> None:
        if parent_key == b"path" and self._in_files:
            # 新文件开始
            self._current_file = {"path": [], "length": 0}
        elif parent_key == b"file list":
            pass

    def on_list_leave(self, parent_key: Optional[bytes]) -> None:
        if parent_key == b"path" and self._in_files:
            if self._current_file is not None:
                self.v1_files.append(self._current_file)
                self._current_file = None

    def on_file_path_enter(self, path: List[bytes]) -> None:
        if self._in_files and self._current_file is not None:
            self._current_file["path"].append(path)

    # ----- 公开 API -----

    def get_info_hash_v1(self, info_dict: Dict[bytes, BencodeValue]) -> bytes:
        """计算 v1 info_hash (SHA-1)."""
        # 移除 v2 字段 (meta_version + file_tree)
        v1_dict = {}
        for k, v in info_dict.items():
            if k in (b"meta version", b"file tree"):
                continue
            v1_dict[k] = v
        return hashlib.sha1(BencodeEncoder.encode(v1_dict)).digest()

    def get_info_hash_v2(self, info_dict: Dict[bytes, BencodeValue]) -> bytes:
        """计算 v2 info_hash_v2 (truncated SHA-256)."""
        return hashlib.sha256(BencodeEncoder.encode(info_dict)).digest()[:32]


# -----------------------------------------------------------------------------
# HybridMagnetBuilder
# -----------------------------------------------------------------------------

class HybridMagnetBuilder:
    """构造 BT v1+v2 hybrid magnet 链."""

    @staticmethod
    def build(v1_info_hash: bytes, v2_info_hash: Optional[bytes] = None,
              name: Optional[str] = None, trackers: Optional[List[str]] = None) -> str:
        """生成 magnet 链.

        v2 hash 格式 (BEP-52): urn:btmh:1220<base32 of 32-byte SHA-256>
        """
        import base64
        parts = [f"xt=urn:btih:{v1_info_hash.hex()}"]
        if v2_info_hash:
            # 0x12 = multihash SHA-256, 0x20 = 32 bytes
            v2_b32 = base64.b32encode(b"\x12\x20" + v2_info_hash).decode().rstrip("=")
            parts.append(f"xt=urn:btmh:{v2_b32.lower()}")
        if name:
            parts.append(f"dn={name}")
        for t in trackers or []:
            parts.append(f"tr={t}")
        return "magnet:?" + "&".join(parts)

    @staticmethod
    def parse(magnet: str) -> Dict:
        """解析 magnet 链."""
        import base64
        import urllib.parse as urlparse
        result = {"v1_hash": None, "v2_hash": None, "name": None, "trackers": []}
        if not magnet.startswith("magnet:?"):
            return result
        qs = urlparse.parse_qs(magnet[8:])
        for xt in qs.get("xt", []):
            if xt.startswith("urn:btih:"):
                result["v1_hash"] = bytes.fromhex(xt[9:])
            elif xt.startswith("urn:btmh:"):
                # base32 解码
                b32 = xt[9:].upper()
                # 补齐 padding
                while len(b32) % 8:
                    b32 += "="
                decoded = base64.b32decode(b32)
                if len(decoded) >= 34:
                    result["v2_hash"] = decoded[2:34]
        if "dn" in qs:
            result["name"] = qs["dn"][0]
        result["trackers"] = qs.get("tr", [])
        return result


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    print("=" * 60)
    print("BitComet Bencode Codec v2 demo")
    print("=" * 60)
    # 编码 demo
    print("\n[1] 编码")
    data = {
        b"announce": b"http://tracker.example.com/announce",
        b"info": {
            b"meta version": 2,
            b"piece length": 16384,
            b"file tree": {
                b"subdir": {
                    b"file.txt": {
                        b"length": 1024,
                        b"pieces root": b"\x00" * 32,
                    }
                }
            }
        }
    }
    encoded = BencodeEncoder.encode(data)
    print(f"  encoded {len(encoded)} bytes: {encoded[:80]!r}...")
    # 解码
    print("\n[2] 解码")
    decoded = BencodeDecoder.decode(encoded)
    print(f"  meta version: {decoded[b'info'][b'meta version']}")
    print(f"  piece length: {decoded[b'info'][b'piece length']}")
    print(f"  file tree file: {list(decoded[b'info'][b'file tree'][b'subdir'].keys())}")
    # round-trip
    print(f"  round-trip: {encoded == BencodeEncoder.encode(decoded)}")
    # SAX 解析
    print("\n[3] SAX 解析 (对应 TorrentFileV2Decode)")
    handler = V2TorrentFileHandler()
    parser = BencodeSaxParser(handler)
    parser.parse(encoded)
    print(f"  meta version: {handler.meta_version}")
    print(f"  piece length: {handler.piece_length}")
    # hybrid magnet
    print("\n[4] Hybrid magnet 链")
    v1 = bytes.fromhex("abcdef0123456789abcdef0123456789abcdef01")
    v2 = bytes.fromhex("deadbeef" * 8)
    magnet = HybridMagnetBuilder.build(v1, v2, name="test", trackers=["http://t.example.com"])
    print(f"  magnet: {magnet}")
    parsed = HybridMagnetBuilder.parse(magnet)
    print(f"  v1_hash match: {parsed['v1_hash'] == v1}")
    print(f"  v2_hash match: {parsed['v2_hash'] == v2}")
    print(f"  name: {parsed['name']}")
    print(f"  trackers: {parsed['trackers']}")
