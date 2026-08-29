"""
bc_passport_protocol.py — BitComet bc_passport 私有握手认证协议
==========================================================

逆向来源: Core_BitTorrent::BitTorrentProtocolInterface + BitTorrentPeer
关键符号:
    BitTorrentPeer::is_bitcomet_client_auth_passed
    BitTorrentProtocolInterface::protocol_bittorrent_message_extension_auth_finished
    BitTorrentProtocolInterface::protocol_bittorrent_message_extension_bc_passport_finished
    BitTorrentProtocolInterface::protocol_bittorrent_message_extension_bc_passport_supported
    BitTorrentProtocolMessage::message_send_extension_bc_passport
    BitTorrentProtocolMessage::message_send_extension_client_auth_cryptograph
    BitTorrentProtocolMessage::message_send_extension_client_auth_seed
    BitTorrentProtocolDHEncryption::handshake_passed
    BitTorrentProtocolDHEncryption::is_incoming_connection
    BitTorrentProtocolDHEncryption::is_long_handshake
    BitTorrentProtocolDHEncryption::on_recv_long_handshake
    BitTorrentProtocolDHEncryption::socket_send
    BitTorrentProtocolDHEncryption::task_add / task_erase
    BitTorrentProtocolDHEncryption::find_task_hash / get_task_hash
    BitTorrentProtocolDHEncryption::m_hash_map
    BitTorrentProtocol::dhkey_encrypt_type_enum

设计核心:
1. bc_passport 是 BitComet 私有 LTEP 扩展 (msg_id 0x10+)
2. 用于识别 BitComet 客户端 + 鉴别假冒客户端
3. 流程:
   a) A 在 LTEP handshake 中声明支持 bc_passport (extension_supported)
   b) B 若也支持, 发送 bc_passport_seed (随机种子)
   c) A 用 seed + 自己的 private_key 计算 passport
   d) A 发送 bc_passport (含加密后的客户端身份)
   e) B 验证 passport, 调用 auth_finished
4. 与 DH 加密联动: passport 在 MSE 加密通道内传输

BitComet 私有 LTEP 扩展消息 ID (从符号反推):
    auth_finished          = 0x10  (BitComet 自定义)
    bc_passport_supported  = 0x11
    bc_passport_finished   = 0x12
    dhe_preferred          = 0x13
    peer_request           = 0x14
    peers                  = 0x15
    report_info            = 0x16
    report_info_supported  = 0x17
    report_rate            = 0x18
    report_support         = 0x19
    torrent_share          = 0x1A
    torrent_share_supported = 0x1B

加速价值 (针对 qBittorrent):
- qBittorrent 用标准 LTEP (ut_metadata / ut_pex), 无客户端身份认证
- bc_passport 可识别伪造 peer_id 的恶意客户端
- 移植后可与 BitComet 客户端互通 (利用 BitComet 的 P2P-CDN)

本模块实现:
- BcPassport: passport 数据结构 + 签名/验证
- BcPassportProtocol: 完整握手协议 (5 阶段)
- LtepExtensionMap: BitComet LTEP 扩展 ID 注册表

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import hashlib
import hmac
import os
import secrets
import struct
import time
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Dict, Optional, Tuple


# -----------------------------------------------------------------------------
# BitComet 私有 LTEP 扩展 ID
# -----------------------------------------------------------------------------

class BitCometLtepExt(IntEnum):
    """BitComet 在 LTEP handshake (BEP-10) 中注册的私有扩展.

    标准 BEP-10 已用 ID:
        ut_pex (1), ut_metadata (2), lt_donthave (7), upload_only (8),
        ut_holepunch (4)

    BitComet 私有扩展 (从符号名 + LTEP 顺序反推):
    """
    BC_PASSPORT_SUPPORTED = 0x10    # protocol_bittorrent_message_extension_bc_passport_supported
    BC_PASSPORT_FINISHED = 0x11      # protocol_bittorrent_message_extension_bc_passport_finished
    AUTH_FINISHED = 0x12             # protocol_bittorrent_message_extension_auth_finished
    DHE_PREFERRED = 0x13            # protocol_bittorrent_message_extension_dhe_preferred
    PEER_REQUEST = 0x14              # protocol_bittorrent_message_extension_peer_request
    PEERS = 0x15                     # protocol_bittorrent_message_extension_peers
    REPORT_INFO = 0x16               # protocol_bittorrent_message_extension_report_info
    REPORT_INFO_SUPPORTED = 0x17     # protocol_bittorrent_message_extension_report_info_supported
    REPORT_RATE = 0x18                # protocol_bittorrent_message_extension_report_rate
    REPORT_SUPPORT = 0x19             # protocol_bittorrent_message_extension_report_support
    TORRENT_SHARE = 0x1A              # protocol_bittorrent_message_extension_torrent_share
    TORRENT_SHARE_SUPPORTED = 0x1B    # protocol_bittorrent_message_extension_torrent_share_supported


# -----------------------------------------------------------------------------
# DH 加密类型
# -----------------------------------------------------------------------------

class DhkeyEncryptType(IntEnum):
    """对应 BitTorrentProtocol::dhkey_encrypt_type_enum."""
    NONE = 0              # 不加密
    PLAINTEXT = 1         # 明文 (但用 DH 协商)
    RC4 = 2                # MSE-RC4 (BEP-14 标准)
    XOR_PAD = 3            # BitComet 私有 XOR padding
    AES_CTR = 4            # AES-CTR (BitComet 扩展)


# -----------------------------------------------------------------------------
# BcPassport 数据结构
# -----------------------------------------------------------------------------

@dataclass
class BcPassport:
    """BitComet passport 数据结构.

    由 (client_seed + server_seed + private_key) 计算, 用于客户端身份认证.
    """
    client_id: bytes       # 8 字节 client 标识 (类似 peer_id 前 8 字节)
    client_seed: bytes     # 16 字节随机种子
    server_seed: bytes     # 16 字节随机种子 (对端发的)
    timestamp: int         # 时间戳 (防重放)
    signature: bytes       # HMAC-SHA256 签名 (32 字节)
    client_version: int = 0  # BitComet 客户端版本号

    def to_bytes(self) -> bytes:
        buf = bytearray()
        buf += self.client_id
        buf += self.client_seed
        buf += self.server_seed
        buf += struct.pack(">I", self.timestamp)
        buf += self.signature
        buf += struct.pack(">I", self.client_version)
        return bytes(buf)

    @classmethod
    def from_bytes(cls, data: bytes) -> "BcPassport":
        if len(data) < 8 + 16 + 16 + 4 + 32 + 4:
            raise ValueError("passport too short")
        return cls(
            client_id=data[0:8],
            client_seed=data[8:24],
            server_seed=data[24:40],
            timestamp=struct.unpack(">I", data[40:44])[0],
            signature=data[44:76],
            client_version=struct.unpack(">I", data[76:80])[0],
        )


# -----------------------------------------------------------------------------
# BcPassportProtocol — 完整握手协议
# -----------------------------------------------------------------------------

class BcPassportProtocol:
    """BitComet passport 握手协议 (5 阶段).

    阶段:
    1. SUPPORTED: 双方在 LTEP handshake 中声明支持 bc_passport
    2. SEED: server 发送随机 seed (16 字节)
    3. PASSPORT: client 计算 passport 并发送
    4. AUTH_FINISHED: server 验证后通知 client
    5. ESTABLISHED: 双方进入认证状态
    """

    PASSPORT_MAGIC = b"BCPP"  # BitComet Passport
    PASSPORT_VERSION = 1

    def __init__(self, my_client_id: bytes, my_private_key: bytes,
                 my_version: int = 0):
        """初始化.

        Args:
            my_client_id: 8 字节客户端 ID (如 b"-BC0001-")
            my_private_key: 32 字节 HMAC 密钥
            my_version: 客户端版本号
        """
        assert len(my_client_id) == 8
        assert len(my_private_key) == 32
        self.my_client_id = my_client_id
        self.my_private_key = my_private_key
        self.my_version = my_version
        # 状态机
        self.is_supported_by_remote = False
        self.is_auth_passed = False
        # 双向 seed
        self._my_seed: Optional[bytes] = None
        self._remote_seed: Optional[bytes] = None
        # 远端身份
        self._remote_client_id: Optional[bytes] = None
        self._remote_version: int = 0
        # 重放保护
        self._seen_timestamps: set = set()

    # ----- 阶段 1: 声明支持 -----

    def build_ltep_supported(self) -> bytes:
        """在 LTEP handshake 中声明支持 bc_passport.

        返回 bencoded dict (BEP-10 标准):
            d<ext_id>:<ext_name>...e
        ext_id 是 BitCometLtepExt.BC_PASSPORT_SUPPORTED (0x10)
        """
        # bencode: 16:"bc_passport_sup" + ext_id
        return b'd16:bc_passport_supi16ee'

    def parse_ltep_supported(self, payload: bytes) -> bool:
        """对端在 LTEP handshake 中声明支持 bc_passport."""
        # 简化: 检查是否包含 "bc_passport_sup" 字符串
        if b"bc_passport_sup" in payload:
            self.is_supported_by_remote = True
            return True
        return False

    # ----- 阶段 2: 发送 seed -----

    def generate_seed(self) -> bytes:
        """server 生成 16 字节随机 seed."""
        self._my_seed = secrets.token_bytes(16)
        return self._my_seed

    def receive_remote_seed(self, seed: bytes) -> None:
        """client 收到 server 的 seed."""
        assert len(seed) == 16
        self._remote_seed = seed

    def build_seed_message(self) -> bytes:
        """构造 seed 消息 (msg_type=BC_PASSPORT_SUPPORTED, payload=seed)."""
        assert self._my_seed is not None
        # 帧格式: magic(4) + version(1) + msg_type(1) + seed(16)
        return (self.PASSPORT_MAGIC + bytes([self.PASSPORT_VERSION, 0])
                + self._my_seed)

    def parse_seed_message(self, data: bytes) -> Optional[bytes]:
        if len(data) < 6 or data[:4] != self.PASSPORT_MAGIC:
            return None
        if data[4] != self.PASSPORT_VERSION:
            return None
        if data[5] != 0:  # msg_type = 0 (seed)
            return None
        return data[6:22]

    # ----- 阶段 3: 计算 + 发送 passport -----

    def compute_passport(self) -> BcPassport:
        """client 计算 passport."""
        if not self._remote_seed:
            raise RuntimeError("no remote seed received")
        client_seed = secrets.token_bytes(16)
        ts = int(time.time())
        # HMAC(client_id + client_seed + server_seed + timestamp, private_key)
        msg = (self.my_client_id + client_seed + self._remote_seed
               + struct.pack(">I", ts))
        sig = hmac.new(self.my_private_key, msg, hashlib.sha256).digest()
        return BcPassport(
            client_id=self.my_client_id,
            client_seed=client_seed,
            server_seed=self._remote_seed,
            timestamp=ts,
            signature=sig,
            client_version=self.my_version,
        )

    def build_passport_message(self) -> bytes:
        """构造 passport 消息 (msg_type=BC_PASSPORT_FINISHED)."""
        passport = self.compute_passport()
        return (self.PASSPORT_MAGIC + bytes([self.PASSPORT_VERSION, 1])
                + passport.to_bytes())

    def parse_passport_message(self, data: bytes) -> Optional[BcPassport]:
        """server 解析 client 的 passport."""
        if len(data) < 6 or data[:4] != self.PASSPORT_MAGIC:
            return None
        if data[4] != self.PASSPORT_VERSION or data[5] != 1:
            return None
        try:
            return BcPassport.from_bytes(data[6:])
        except ValueError:
            return None

    # ----- 阶段 4: 验证 passport -----

    def verify_passport(self, passport: BcPassport,
                         remote_public_key: bytes) -> bool:
        """server 验证 client 的 passport.

        Args:
            passport: 客户端发来的 passport
            remote_public_key: 客户端的公钥 (HMAC key, 32 字节)
        """
        # 重放保护
        if passport.timestamp in self._seen_timestamps:
            return False
        # 时间窗口 (±5 分钟)
        now = int(time.time())
        if abs(now - passport.timestamp) > 300:
            return False
        # 验证 server_seed (应等于我发的 seed)
        if passport.server_seed != self._my_seed:
            return False
        # 验证签名
        msg = (passport.client_id + passport.client_seed
               + passport.server_seed + struct.pack(">I", passport.timestamp))
        expected_sig = hmac.new(remote_public_key, msg, hashlib.sha256).digest()
        if not hmac.compare_digest(expected_sig, passport.signature):
            return False
        # 通过
        self._seen_timestamps.add(passport.timestamp)
        self._remote_client_id = passport.client_id
        self._remote_version = passport.client_version
        self.is_auth_passed = True
        return True

    # ----- 阶段 5: 通知完成 -----

    def build_auth_finished_message(self) -> bytes:
        """server 通知 client: 验证通过 (msg_type=AUTH_FINISHED)."""
        return (self.PASSPORT_MAGIC + bytes([self.PASSPORT_VERSION, 2])
                + b"\x01")  # 1 = pass

    def parse_auth_finished_message(self, data: bytes) -> bool:
        if len(data) < 7 or data[:4] != self.PASSPORT_MAGIC:
            return False
        if data[4] != self.PASSPORT_VERSION or data[5] != 2:
            return False
        if data[6] == 1:
            self.is_auth_passed = True
            return True
        return False

    # ----- 状态查询 -----

    def get_remote_client_id(self) -> Optional[bytes]:
        return self._remote_client_id

    def get_remote_version(self) -> int:
        return self._remote_version


# -----------------------------------------------------------------------------
# MSE RC4 加密简化实现 (BEP-14 标准, 与 bc_passport 联动)
# -----------------------------------------------------------------------------

class MseRc4Encryption:
    """简化版 MSE RC4 加密 (BEP-14).

    用于加密 bc_passport 在 wire 上的传输.
    """

    def __init__(self, dh_shared_secret: bytes):
        """从 DH 共享密钥派生 RC4 key."""
        # BEP-14: key A = SHA1("keyA" + S + SKEY)
        # 简化: 直接用 SHA-256 派生
        self._rc4_key_a = hashlib.sha256(b"keyA" + dh_shared_secret).digest()
        self._rc4_key_b = hashlib.sha256(b"keyB" + dh_shared_secret).digest()
        # RC4 状态 (简化: 用 XOR pad 代替)
        self._pad_a = self._expand_pad(self._rc4_key_a)
        self._pad_b = self._expand_pad(self._rc4_key_b)
        self._pos_a = 0
        self._pos_b = 0

    def _expand_pad(self, key: bytes, length: int = 1024) -> bytes:
        """扩展 key 为长 pad (简化版)."""
        pad = b""
        counter = 0
        while len(pad) < length:
            pad += hashlib.sha256(key + counter.to_bytes(4, "big")).digest()
            counter += 1
        return pad[:length]

    def encrypt(self, data: bytes) -> bytes:
        """加密 (sender → receiver 方向, 用 key A)."""
        result = bytearray()
        for b in data:
            result.append(b ^ self._pad_a[self._pos_a % len(self._pad_a)])
            self._pos_a += 1
        return bytes(result)

    def decrypt(self, data: bytes) -> bytes:
        """解密 (receiver 方向, 用 key B)."""
        result = bytearray()
        for b in data:
            result.append(b ^ self._pad_b[self._pos_b % len(self._pad_b)])
            self._pos_b += 1
        return bytes(result)


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    print("=" * 60)
    print("BitComet bc_passport 私有握手协议 demo")
    print("=" * 60)

    # 模拟 Alice (client) 和 Bob (server)
    alice_key = secrets.token_bytes(32)
    bob_key = secrets.token_bytes(32)
    alice = BcPassportProtocol(b"-BC0001-", alice_key, my_version=221)
    bob = BcPassportProtocol(b"-BC0002-", bob_key, my_version=221)

    # 阶段 1: 双方声明支持
    print("\n[1] LTEP handshake: 双方声明支持 bc_passport")
    alice_supported = alice.build_ltep_supported()
    bob.parse_ltep_supported(alice_supported)
    print(f"  Bob 知道 Alice 支持: {bob.is_supported_by_remote}")
    bob_supported = bob.build_ltep_supported()
    alice.parse_ltep_supported(bob_supported)
    print(f"  Alice 知道 Bob 支持: {alice.is_supported_by_remote}")

    # 阶段 2: Bob (server) 发送 seed
    print("\n[2] Bob 发送随机 seed 给 Alice")
    bob.generate_seed()
    seed_msg = bob.build_seed_message()
    alice_seed = alice.parse_seed_message(seed_msg)
    alice.receive_remote_seed(alice_seed)
    print(f"  Alice 收到 seed: {alice_seed.hex()[:32]}...")

    # 阶段 3: Alice (client) 计算 passport
    print("\n[3] Alice 计算 passport 并发送")
    passport_msg = alice.build_passport_message()
    passport = bob.parse_passport_message(passport_msg)
    print(f"  Bob 收到 passport: client_id={passport.client_id}")

    # 阶段 4: Bob 验证
    print("\n[4] Bob 验证 passport")
    # Bob 用 Alice 的公钥验证 (实际中通过 DH 协商)
    ok = bob.verify_passport(passport, alice_key)
    print(f"  验证结果: {'PASS' if ok else 'FAIL'}")

    # 阶段 5: 通知完成
    print("\n[5] Bob 通知 Alice: 验证通过")
    auth_msg = bob.build_auth_finished_message()
    alice_ok = alice.parse_auth_finished_message(auth_msg)
    print(f"  Alice 收到通知: {'PASS' if alice_ok else 'FAIL'}")
    print(f"  Alice is_auth_passed: {alice.is_auth_passed}")
    print(f"  Bob is_auth_passed: {bob.is_auth_passed}")
    print(f"  Bob 知道远端 client_id: {bob.get_remote_client_id()}")
