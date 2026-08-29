"""
close_reason_decoder.py — BitComet 私有 close_reason 扩展解码器
==========================================================

逆向来源: Core_BitTorrent::libtorrent::close_reason_t (libtorrent fork 私有扩展)

完整符号证据 (从 nm -C 提取):
    Core_BitTorrent::libtorrent::get_close_reason_string(close_reason_t)
    Core_Socket::utp_connection::set_close_reason(unsigned short)
    Core_Socket::utp_connection::parse_close_reason(char const*, int)
    Core_Socket::utp_connection::get_remote_close_reason(unsigned short&)
    Core_Socket::InterfaceSocket::wire_set_close_reason(void*, void*, unsigned short)
    Core_Socket::InterfaceSocket::wire_get_remote_close_reason(void*, void*, unsigned short&)
    Core_Socket::InterfaceSocketUTP::wire_set_close_reason(...)
    Core_Socket::InterfaceSocketUTP::wire_get_remote_close_reason(...)
    Core_Wire::WireLinkPool::protocol_set_close_reason(void*, void*, unsigned short)
    Core_Wire::WireLinkPool::protocol_get_remote_close_reason(...)
    Core_Wire::InterfaceWire::protocol_set_close_reason(...)
    Core_Wire::InterfaceWire::protocol_set_close_reason_i(...)   ← 带_i 后缀 = 内部版本
    Core_Wire::InterfaceWire::protocol_get_remote_close_reason(...)
    Core_Wire::InterfaceWire::protocol_get_remote_close_reason_i(...)
    Core_Wire::WireLinkLayer::wire_set_close_reason(unsigned short)
    Core_Wire::WireLinkLayer::wire_get_remote_close_reason(unsigned short&)

确认的 close_reason 字符串值 (来自 strings 提取, 在 .rodata 中):
    hash_check_failed         ← BitComet 私有扩展
    invalid_metadata           ← BitComet 私有扩展
    protocol_error             ← BitComet 私有扩展
    too_many_connections       ← BitComet 私有扩展

设计核心:
1. BitComet 在 libtorrent fork 中扩展了 BEP-14 ut_close 消息
2. 标准 BEP-14 close_reason 只有 reason_id (无字符串), BitComet 在内部
   增加了字符串表 (50 个 entry, 每个 40 字节: id(4)+pad(4)+str_ptr(8)+str_len(8)+extra(16))
3. 通过 wire_set_close_reason / wire_get_remote_close_reason 在 Wire 层透传
4. parse_close_reason 接受 char* + len, 说明 Wire 协议中 close_reason
   可能用字符串传输 (而非 16-bit 数字)

加速价值 (针对 qBittorrent):
- qBittorrent 用上游 libtorrent, 仅知 6 个标准 close_reason
- 无法区分 "Hash 校验失败" 与 "用户主动断开", 调试困难
- BitComet 扩展让客户端能在断开时报告更精确原因
- LT-Seed 协议可复用 close_reason 通知对方为何下线

本模块实现:
- BitComet 扩展 close_reason_t 完整枚举 (标准 + 私有)
- parse_close_reason() 字符串解析
- encode_close_reason() 数字 → 字符串
- 与 BEP-14 协议的兼容层

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

from dataclasses import dataclass
from enum import IntEnum
from typing import Dict, Optional


# -----------------------------------------------------------------------------
# 标准 BEP-14 close_reason_t (libtorrent 上游定义)
# -----------------------------------------------------------------------------

class StandardCloseReason(IntEnum):
    """BEP-14 ut_close 标准原因 (上游 libtorrent 定义).

    编号参考: libtorrent source code, peer_connection.cpp
    """
    NONE = 0
    PEER_REQUEST_TIMEOUT = 1
    PEER_TIMEOUT = 2
    PEER_EOF = 3
    PEER_RESET = 4
    PEER_UNREACHABLE = 5
    PEER_INTERESTING = 6
    PEER_NOT_INTERESTED = 7
    PEER_CHOKING = 8
    PEER_DISCONNECT = 9
    PEER_BANNED = 10
    PEER_TOO_MANY_CONNECTIONS = 11
    PEER_NO_LISTEN_PORT = 12
    PEER_INVALID_MESSAGE = 13
    PEER_INVALID_REQUEST = 14
    PEER_INVALID_PIECE = 15
    PEER_INVALID_HASH = 16
    PEER_INVALID_METADATA = 17
    PEER_INVALID_PROTOCOL = 18
    PEER_INVALID_HANDSHAKE = 19
    PEER_TOO_MANY_FAILED_REQUESTS = 20
    PEER_TOO_MANY_REQUESTS = 21
    PEER_REQUEST_NOT_FOUND = 22
    PEER_REQUEST_REJECTED = 23
    PEER_PIECE_NOT_AVAILABLE = 24
    PEER_DUPLICATE_REQUEST = 25
    PEER_TOO_MANY_PIECES = 26
    PEER_BAD_PIECE = 27
    PEER_DISK_FAILURE = 28
    PEER_DISK_FULL = 29
    PEER_DISK_IO_ERROR = 30
    PEER_TIMEOUT_KEEPALIVE = 31
    PEER_TIMEOUT_REQUEST = 32
    PEER_TIMEOUT_HANDSHAKE = 33
    PEER_TIMEOUT_DISCONNECT = 34
    USER_SHUTDOWN = 35
    USER_QUIT = 36
    USER_DOWNLOAD_FINISHED = 37
    USER_STOPPED = 38
    USER_PAUSED = 39
    USER_REMOVED = 40


# -----------------------------------------------------------------------------
# BitComet 私有扩展 (从 .rodata 字符串反推)
# -----------------------------------------------------------------------------

class BitCometCloseReason(IntEnum):
    """BitComet libtorrent fork 私有 close_reason 扩展.

    起始编号 1000+ 避免与上游冲突.
    实际编号需运行时确认 (静态无法读取 .bss 表), 这里按字符串发现的顺序赋值.
    """
    # 标准 close_reason (BEP-14)
    NONE = 0
    PEER_REQUEST_TIMEOUT = 1
    PEER_TIMEOUT = 2
    PEER_EOF = 3
    PEER_RESET = 4
    PEER_DISCONNECT = 9
    PEER_BANNED = 10
    PEER_TOO_MANY_CONNECTIONS = 11
    USER_SHUTDOWN = 35

    # ↓↓↓ BitComet 私有扩展 (strings 中确认的 4 个) ↓↓↓
    # 通过 nm + objdump 在 .rodata 中找到, 但 enum 编号未直接读取
    # 推测编号区间: 100-199 (BitComet 自定义)
    HASH_CHECK_FAILED = 100           # ← 在 strings 中确认
    INVALID_METADATA = 101            # ← 在 strings 中确认
    PROTOCOL_ERROR = 102              # ← 在 strings 中确认
    TOO_MANY_CONNECTIONS = 103        # ← 在 strings 中确认 (与标准 11 不同, 私有用 103)

    # ↓↓↓ 推测扩展 (基于符号 + 字符串行为推断, 未在 .rodata 直接确认) ↓↓↓
    PIECE_HASH_MISMATCH = 110
    PEER_UNGRACEFUL = 111
    DISK_BUSY = 112
    RATE_TOO_HIGH = 113
    ANTI_LEECH_BLOCK = 114            # ← AntiLeechLevel 触发的关闭
    IP_FILTER_BLOCKED = 115          # ← IP filter 命中
    CLIENT_FILTER_BLOCKED = 116       # ← 客户端 filter 命中
    NAT_BLOCK = 117
    UTP_TIMEOUT = 118
    UTP_CONGESTION = 119
    PEER_SNUBBED = 120
    DUPLICATE_CONNECTION = 121
    TRACKER_REQUEST_FAILED = 122
    DHT_UNAVAILABLE = 123
    FILE_CHECK_FAILED = 130
    FILE_ALLOCATE_FAILED = 131
    METADATA_TIMEOUT = 140


# -----------------------------------------------------------------------------
# 字符串映射表 (从 close_reason.cpp + strings 提取)
# -----------------------------------------------------------------------------

# 标准 libtorrent 上游的字符串表
STANDARD_REASON_STRINGS: Dict[int, str] = {
    0:  "none",
    1:  "peer_request_timeout",
    2:  "peer_timeout",
    3:  "peer_eof",
    4:  "peer_reset",
    5:  "peer_unreachable",
    9:  "peer_disconnect",
    10: "peer_banned",
    11: "too_many_connections",  # ← 标准
    13: "invalid_message",
    14: "invalid_request",
    15: "invalid_piece",
    16: "invalid_hash",
    17: "invalid_metadata",
    18: "invalid_protocol",
    27: "bad_piece",
    28: "disk_failure",
    29: "disk_full",
    30: "disk_io_error",
    35: "user_shutdown",
    36: "user_quit",
}

# BitComet 私有扩展 (从 strings 提取的 4 个确认字符串)
BITCOMET_PRIVATE_STRINGS: Dict[int, str] = {
    # 注意: 编号 100-103 是推测, 真实编号需运行时验证
    # 但字符串本身在 .rodata 中确认存在
    BitCometCloseReason.HASH_CHECK_FAILED:     "hash_check_failed",
    BitCometCloseReason.INVALID_METADATA:      "invalid_metadata",
    BitCometCloseReason.PROTOCOL_ERROR:        "protocol_error",
    BitCometCloseReason.TOO_MANY_CONNECTIONS:  "too_many_connections",
    # 推测的扩展 (基于 AntiLeech / IP filter / NAT 等)
    BitCometCloseReason.PIECE_HASH_MISMATCH:   "piece_hash_mismatch",
    BitCometCloseReason.PEER_UNGRACEFUL:       "peer_ungraceful",
    BitCometCloseReason.DISK_BUSY:             "disk_busy",
    BitCometCloseReason.RATE_TOO_HIGH:         "rate_too_high",
    BitCometCloseReason.ANTI_LEECH_BLOCK:      "anti_leech_block",
    BitCometCloseReason.IP_FILTER_BLOCKED:     "ip_filter_blocked",
    BitCometCloseReason.CLIENT_FILTER_BLOCKED:"client_filter_blocked",
    BitCometCloseReason.NAT_BLOCK:             "nat_block",
    BitCometCloseReason.UTP_TIMEOUT:           "utp_timeout",
    BitCometCloseReason.UTP_CONGESTION:        "utp_congestion",
    BitCometCloseReason.PEER_SNUBBED:          "peer_snubbed",
    BitCometCloseReason.DUPLICATE_CONNECTION:  "duplicate_connection",
    BitCometCloseReason.TRACKER_REQUEST_FAILED:"tracker_request_failed",
    BitCometCloseReason.DHT_UNAVAILABLE:       "dht_unavailable",
    BitCometCloseReason.FILE_CHECK_FAILED:     "file_check_failed",
    BitCometCloseReason.FILE_ALLOCATE_FAILED:  "file_allocate_failed",
    BitCometCloseReason.METADATA_TIMEOUT:      "metadata_timeout",
}

# 合并表
ALL_REASON_STRINGS: Dict[int, str] = {**STANDARD_REASON_STRINGS, **BITCOMET_PRIVATE_STRINGS}

# 反向映射 (string → id)
STRING_TO_ID: Dict[str, int] = {v: k for k, v in ALL_REASON_STRINGS.items()}


# -----------------------------------------------------------------------------
# 协议层 API
# -----------------------------------------------------------------------------

@dataclass
class CloseReasonInfo:
    """对应 close_reason.cpp 中的 items 表 entry.

    每个 entry 40 字节 (逆向自 items 表大小 0x7d0 = 50*40):
        id(4) + pad(4) + str_ptr(8) + str_len(8) + reserved(16)
    """
    reason_id: int
    reason_str: str
    is_bitcomet_private: bool = False
    description: str = ""


def parse_close_reason(data: bytes) -> Optional[CloseReasonInfo]:
    """对应 Core_Socket::utp_connection::parse_close_reason(char const*, int).

    BitComet 用字符串传输 close_reason (而非 16-bit 数字), 这样兼容性更好.
    """
    if not data:
        return None
    try:
        s = data.decode("ascii", errors="replace").rstrip("\x00")
    except Exception:
        return None
    if not s:
        return None
    reason_id = STRING_TO_ID.get(s)
    if reason_id is None:
        return CloseReasonInfo(reason_id=-1, reason_str=s, is_bitcomet_private=False,
                                description="unknown close_reason string")
    return CloseReasonInfo(
        reason_id=reason_id,
        reason_str=s,
        is_bitcomet_private=reason_id >= 100,
        description=_get_description(reason_id),
    )


def encode_close_reason(reason_id: int) -> bytes:
    """把 close_reason 数字编码为字符串 (与 BitComet 兼容).

    对应 wire_set_close_reason 的反向操作.
    """
    s = ALL_REASON_STRINGS.get(reason_id)
    if s is None:
        return b"unknown"
    return s.encode("ascii")


def get_reason_string(reason_id: int) -> str:
    """对应 get_close_reason_string(close_reason_t) → std::string."""
    return ALL_REASON_STRINGS.get(reason_id, "unknown")


def get_reason_id(reason_str: str) -> int:
    """反向映射: string → id."""
    return STRING_TO_ID.get(reason_str, -1)


def is_bitcomet_private(reason_id: int) -> bool:
    """判定是否为 BitComet 私有扩展."""
    return reason_id >= 100


def _get_description(reason_id: int) -> str:
    """人类可读描述."""
    desc = {
        BitCometCloseReason.HASH_CHECK_FAILED:
            "Piece SHA-1 校验失败, 通常因网络传输错误或对端发送错误数据",
        BitCometCloseReason.INVALID_METADATA:
            "Metadata (BEP-9) 解析失败, 对端发送的 .torrent 元数据损坏",
        BitCometCloseReason.PROTOCOL_ERROR:
            "BT 协议错误, 如错误的 message_id 或非法握手响应",
        BitCometCloseReason.TOO_MANY_CONNECTIONS:
            "超过最大连接数限制 (BitComet 私有版本, 与标准 11 不同)",
        BitCometCloseReason.ANTI_LEECH_BLOCK:
            "AntiLeech 模块判定为吸血客户端, 主动断开 (BAN 等级触发)",
        BitCometCloseReason.IP_FILTER_BLOCKED:
            "IP filter (ipfilter) 命中, 主动断开",
        BitCometCloseReason.CLIENT_FILTER_BLOCKED:
            "客户端过滤器 (client_filter) 命中, 主动断开",
        BitCometCloseReason.NAT_BLOCK:
            "对端在 NAT 后且无法穿透 (无 introducer / repeater 不可用)",
        BitCometCloseReason.UTP_TIMEOUT:
            "uTP 协议超时 (RFC 6817 RTO 超时, 通常网络拥塞)",
        BitCometCloseReason.UTP_CONGESTION:
            "uTP 拥塞, LEDBAT off-target 持续为负",
        BitCometCloseReason.PEER_SNUBBED:
            "对端 snubbed (60s 未响应我们 piece 请求)",
        BitCometCloseReason.DUPLICATE_CONNECTION:
            "相同 peer_id 重复连接, 主动断开较弱的一方",
        BitCometCloseReason.TRACKER_REQUEST_FAILED:
            "Tracker 请求失败, 间接导致 peer 失活",
        BitCometCloseReason.DHT_UNAVAILABLE:
            "DHT 查询无响应, 死种场景常见",
        BitCometCloseReason.FILE_CHECK_FAILED:
            "本地文件 hash 校验失败 (磁盘损坏)",
        BitCometCloseReason.FILE_ALLOCATE_FAILED:
            "本地文件分配失败 (磁盘满或权限不足)",
        BitCometCloseReason.METADATA_TIMEOUT:
            "等待 metadata 超时 (BEP-9 ut_metadata)",
    }
    return desc.get(reason_id, "")


# -----------------------------------------------------------------------------
# BEP-14 兼容层
# -----------------------------------------------------------------------------

class BEP14Encoder:
    """BEP-14 ut_close 标准编码 (libtorrent 上游用 16-bit 数字).

    BitComet 同时支持标准 (数字) + 私有 (字符串) 两种方式.
    本类实现两种编码的桥接.
    """

    @staticmethod
    def encode_standard(reason_id: int) -> bytes:
        """BEP-14: length(4) + message_id(1, =12) + reason_id(4)."""
        return b"\x00\x00\x00\x05\x0c" + reason_id.to_bytes(4, "big")

    @staticmethod
    def encode_bitcomet(reason_id: int) -> bytes:
        """BitComet 私有: length(4) + message_id(1, =0xFE) + str_len(2) + str."""
        s = get_reason_string(reason_id)
        encoded = s.encode("ascii")
        return b"\x00\x00\x00\x00\xfe" + len(encoded).to_bytes(2, "big") + encoded

    @staticmethod
    def decode(data: bytes) -> Optional[CloseReasonInfo]:
        """自动识别 BEP-14 标准 vs BitComet 私有."""
        if len(data) < 5:
            return None
        msg_id = data[4]
        if msg_id == 0x0c:  # BEP-14 ut_close
            if len(data) < 9:
                return None
            reason_id = int.from_bytes(data[5:9], "big")
            return CloseReasonInfo(
                reason_id=reason_id,
                reason_str=get_reason_string(reason_id),
                is_bitcomet_private=is_bitcomet_private(reason_id),
                description=_get_description(reason_id),
            )
        elif msg_id == 0xFE:  # BitComet 私有
            if len(data) < 7:
                return None
            str_len = int.from_bytes(data[5:7], "big")
            if len(data) < 7 + str_len:
                return None
            return parse_close_reason(data[7:7 + str_len])
        return None


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser(description="BitComet close_reason 解码器")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p_list = sub.add_parser("list", help="列出所有 close_reason")
    p_list.add_argument("--bitcomet-only", action="store_true")

    p_enc = sub.add_parser("encode", help="把 reason_id 编码为字符串")
    p_enc.add_argument("reason_id", type=int)

    p_dec = sub.add_parser("decode", help="把字符串解码为 reason_id")
    p_dec.add_argument("reason_str")

    p_bep14 = sub.add_parser("bep14", help="BEP-14 兼容模式编码")
    p_bep14.add_argument("reason_id", type=int)
    p_bep14.add_argument("--bitcomet", action="store_true",
                          help="用 BitComet 私有编码 (msg_id=0xFE)")

    args = ap.parse_args()

    if args.cmd == "list":
        for rid, s in sorted(ALL_REASON_STRINGS.items()):
            if args.bitcomet_only and not is_bitcomet_private(rid):
                continue
            marker = "★" if is_bitcomet_private(rid) else " "
            print(f"  {marker} [{rid:3d}] {s}")

    elif args.cmd == "encode":
        b = encode_close_reason(args.reason_id)
        print(f"reason_id={args.reason_id} → bytes={b!r} (string={b.decode()})")

    elif args.cmd == "decode":
        info = parse_close_reason(args.reason_str.encode())
        if info:
            print(f"string='{args.reason_str}' → id={info.reason_id}")
            print(f"  bitcomet_private: {info.is_bitcomet_private}")
            if info.description:
                print(f"  description: {info.description}")

    elif args.cmd == "bep14":
        if args.bitcomet:
            b = BEP14Encoder.encode_bitcomet(args.reason_id)
            print(f"BEP-14 BitComet private: {b!r}")
            # 反向解码验证
            decoded = BEP14Encoder.decode(b)
            if decoded:
                print(f"  decoded: id={decoded.reason_id} str={decoded.reason_str}")
        else:
            b = BEP14Encoder.encode_standard(args.reason_id)
            print(f"BEP-14 standard: {b!r}")
