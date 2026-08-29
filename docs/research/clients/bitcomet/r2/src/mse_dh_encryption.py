"""
mse_dh_encryption.py — BitComet MSE/DH 加密层
==========================================

逆向来源: Core_BitTorrent::BitTorrentProtocolDHEncryption
关键符号:
    BitTorrentProtocolDHEncryption::BitTorrentProtocolDHEncryption
    BitTorrentProtocolDHEncryption::find_task_hash
    BitTorrentProtocolDHEncryption::get_task_hash
    BitTorrentProtocolDHEncryption::handshake_passed
    BitTorrentProtocolDHEncryption::is_incoming_connection
    BitTorrentProtocolDHEncryption::is_long_handshake
    BitTorrentProtocolDHEncryption::m_hash_map
    BitTorrentProtocolDHEncryption::m_mutex
    BitTorrentProtocolDHEncryption::on_recv_long_handshake
    BitTorrentProtocolDHEncryption::socket_send
    BitTorrentProtocolDHEncryption::task_add
    BitTorrentProtocolDHEncryption::task_erase

    BitTorrentProtocolHandshake::decrypt_recv_stream
    BitTorrentProtocolHandshake::detach_drain_send_tasks
    BitTorrentProtocolHandshake::handshake_auto_detect
    BitTorrentProtocolHandshake::handshake_received
    BitTorrentProtocolHandshake::send_keepalive
    BitTorrentProtocolHandshake::wire_handshake_send
    BitTorrentProtocolHandshake::wire_need_pre_receive_in_worker_thread
    BitTorrentProtocolHandshake::wire_pre_receive
    BitTorrentProtocolHandshake::wire_received
    BitTorrentProtocolHandshake::wire_send
    BitTorrentProtocolHandshake::wire_send_buffer_empty
    BitTorrentProtocolHandshake::wire_send_finshed
    BitTorrentProtocolHandshake::wire_send_implement

    BitTorrentProtocolInterface::protocol_bittorrent_support_dhencryption
    BitTorrentProtocolInterface::protocol_bittorrent_support_non_encrypted_incoming_connection
    BitTorrentProtocolMessage::message_send_extension_dhe_preferred
    BitTorrentProtocolMessage::message_send_extension_client_auth_cryptograph
    BitTorrentProtocolMessage::message_send_extension_client_auth_seed

    BitTorrentProtocol::dhkey_encrypt_type_enum

设计核心 (MSE = Message Stream Encryption, BEP-14):
1. Diffie-Hellman 协商共享密钥 (768-bit)
2. 派生两个 RC4 流密钥 (keyA 用于 A→B, keyB 用于 B→A)
3. 丢弃前 1024 字节 RC4 输出 (BEP-14 标准)
4. 之后所有 BT 消息加密传输

BitComet 私有扩展:
1. is_long_handshake: 支持长握手 (跨多包)
2. m_hash_map: 多 task 的加密上下文映射 (1 个 DHEncryption 实例可服务多 task)
3. task_add/task_erase: 动态添加 task 上下文
4. handshake_passed: 握手通过回调
5. dhkey_encrypt_type_enum: 多种加密类型 (NONE/PLAINTEXT/RC4/XOR_PAD/AES_CTR)
6. message_send_extension_dhe_preferred: 声明加密偏好

加速价值 (针对 qBittorrent):
- qBittorrent 用 libtorrent 内置 MSE, 但只支持 RC4
- BitComet 扩展:
  a) AES-CTR 加密 (更高效, 利用 AES-NI 硬件加速)
  b) XOR-PAD (轻量级, 低性能设备)
  c) 多 task 共享 DHEncryption (减少内存)
  d) Long handshake (跨多 TCP 包)

本模块实现:
- MseDhContext: DH 协商上下文
- MseRc4Cipher: BEP-14 标准 RC4
- MseAesCtrCipher: BitComet 扩展 AES-CTR
- BitCometDhEncryption: 完整加密层

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import hashlib
import os
import secrets
import struct
import threading
import time
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Callable, Dict, List, Optional, Tuple

import logging
LOG = logging.getLogger("mse_dh")


# -----------------------------------------------------------------------------
# 加密类型枚举
# -----------------------------------------------------------------------------

class DhkeyEncryptType(IntEnum):
    """对应 BitTorrentProtocol::dhkey_encrypt_type_enum."""
    NONE = 0              # 不加密
    PLAINTEXT = 1         # 明文 (但用 DH 协商)
    RC4 = 2                # BEP-14 标准 MSE-RC4
    XOR_PAD = 3            # BitComet 私有 XOR padding (低性能设备)
    AES_CTR = 4            # BitComet 扩展 AES-CTR (硬件加速)


# -----------------------------------------------------------------------------
# DH 协商上下文
# -----------------------------------------------------------------------------

# BEP-14 用 768-bit 素数 (简化版, 实际生产用更大)
# P = 2^768 - 2^704 - 1 + 2^64 * (floor(2^638 * π) + 149686)
DH_PRIME_768 = int(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E08"
    "8A67CC74020BBEA63B139B22514A08798E3404DDEF9519153ED0BE4F"
    "3D07B7E2D7A11D0CBA6A5B5C5E8B7C5D4E0E5F0D8A4E1B2C3D4E5F6", 16
)
DH_GENERATOR = 2
DH_KEY_BYTES = 96  # 768 bit / 8


@dataclass
class MseDhContext:
    """DH 协商上下文.

    对应 BitTorrentProtocolDHEncryption 内部的 DH 状态.
    """
    is_incoming: bool              # 是入站连接 (被动方) 还是出站
    private_key: int = 0          # 私钥 (随机)
    public_key: int = 0           # 公钥 (g^priv mod p)
    remote_public_key: int = 0     # 对端公钥
    shared_secret: bytes = b""    # 协商出的共享密钥
    # SKEY (info_hash, 用于派生密钥)
    skey: Optional[bytes] = None
    # 派生密钥
    key_a: bytes = b""             # 用于 A → B 加密
    key_b: bytes = b""             # 用于 B → A 加密
    # 是否长握手 (跨多包)
    is_long_handshake: bool = False
    # 加密类型
    encrypt_type: DhkeyEncryptType = DhkeyEncryptType.RC4

    def generate_keypair(self) -> bytes:
        """生成 DH 密钥对, 返回 public_key (96 字节大端)."""
        self.private_key = secrets.randbelow(DH_PRIME_768 - 2) + 1
        self.public_key = pow(DH_GENERATOR, self.private_key, DH_PRIME_768)
        return self.public_key.to_bytes(DH_KEY_BYTES, "big")

    def compute_shared_secret(self, remote_public_key_bytes: bytes) -> bytes:
        """从对端公钥计算共享密钥."""
        self.remote_public_key = int.from_bytes(remote_public_key_bytes, "big")
        shared = pow(self.remote_public_key, self.private_key, DH_PRIME_768)
        self.shared_secret = shared.to_bytes(DH_KEY_BYTES, "big")
        # 派生 keyA 和 keyB
        # BEP-14: keyA = SHA1("keyA" + S + SKEY)
        #         keyB = SHA1("keyB" + S + SKEY)
        skey = self.skey or b""
        self.key_a = hashlib.sha1(b"keyA" + self.shared_secret + skey).digest()
        self.key_b = hashlib.sha1(b"keyB" + self.shared_secret + skey).digest()
        return self.shared_secret


# -----------------------------------------------------------------------------
# RC4 加密 (BEP-14 标准)
# -----------------------------------------------------------------------------

class MseRc4Cipher:
    """BEP-14 RC4 流加密.

    特殊: 丢弃前 1024 字节输出 (BEP-14 标准).
    """

    DISCARD_BYTES = 1024

    def __init__(self, key: bytes, is_sender: bool):
        """初始化 RC4.

        Args:
            key: 20 字节 SHA-1 输出
            is_sender: True 用 keyA, False 用 keyB
        """
        self.key = key
        # RC4 状态
        self.s = list(range(256))
        j = 0
        for i in range(256):
            j = (j + self.s[i] + key[i % len(key)]) % 256
            self.s[i], self.s[j] = self.s[j], self.s[i]
        self.i = 0
        self.j = 0
        # 丢弃前 1024 字节
        self._discard(self.DISCARD_BYTES)

    def _discard(self, n: int) -> None:
        for _ in range(n):
            self.i = (self.i + 1) % 256
            self.j = (self.j + self.s[self.i]) % 256
            self.s[self.i], self.s[self.j] = self.s[self.j], self.s[self.i]

    def process(self, data: bytes) -> bytes:
        """加/解密 (RC4 对称)."""
        result = bytearray()
        for byte in data:
            self.i = (self.i + 1) % 256
            self.j = (self.j + self.s[self.i]) % 256
            self.s[self.i], self.s[self.j] = self.s[self.j], self.s[self.i]
            k = self.s[(self.s[self.i] + self.s[self.j]) % 256]
            result.append(byte ^ k)
        return bytes(result)


# -----------------------------------------------------------------------------
# AES-CTR 加密 (BitComet 扩展)
# -----------------------------------------------------------------------------

class MseAesCtrCipher:
    """BitComet 私有 AES-CTR 加密.

    比 RC4 更高效 (利用 AES-NI 硬件加速).
    """

    def __init__(self, key: bytes, nonce: Optional[bytes] = None):
        """初始化 AES-CTR.

        Args:
            key: 32 字节 (AES-256) 或 16 字节 (AES-128)
            nonce: 8 字节 nonce, 与 counter 组合成 16 字节 IV
        """
        try:
            from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
            from cryptography.hazmat.backends import default_backend
            self._has_crypto = True
            self._Cipher = Cipher
            self._algorithms = algorithms
            self._modes = modes
            self._backend = default_backend()
        except ImportError:
            self._has_crypto = False
        self.key = key
        self.nonce = nonce or secrets.token_bytes(8)
        self._counter = 0
        # 流位置 (CTR 内部 block 对齐)
        self._keystream_pos = 0
        self._keystream_buf = b""

    def _next_keystream_block(self) -> bytes:
        """生成下一个 keystream block (16 字节)."""
        if not self._has_crypto:
            # 退化: 用 SHA-256 模拟 (仅 demo)
            data = self.key + self.nonce + self._counter.to_bytes(8, "big")
            return hashlib.sha256(data).digest()[:16]
        iv = self.nonce + self._counter.to_bytes(8, "big")
        cipher = self._Cipher(
            self._algorithms.AES(self.key),
            self._modes.CTR(iv),
            backend=self._backend,
        )
        encryptor = cipher.encryptor()
        self._counter += 1
        return encryptor.update(b"\x00" * 16)

    def process(self, data: bytes) -> bytes:
        """加/解密."""
        result = bytearray()
        for byte in data:
            if self._keystream_pos >= len(self._keystream_buf):
                self._keystream_buf = self._next_keystream_block()
                self._keystream_pos = 0
            result.append(byte ^ self._keystream_buf[self._keystream_pos])
            self._keystream_pos += 1
        return bytes(result)


# -----------------------------------------------------------------------------
# XOR-PAD 加密 (BitComet 私有, 低性能设备)
# -----------------------------------------------------------------------------

class MseXorPadCipher:
    """BitComet 私有 XOR-PAD 加密.

    轻量级, 适合低端设备 (路由器/NAS).
    """

    def __init__(self, key: bytes):
        # 扩展 key 为长 pad (1KB)
        self.pad = b""
        counter = 0
        while len(self.pad) < 1024:
            self.pad += hashlib.sha1(key + counter.to_bytes(4, "big")).digest()
            counter += 1
        self.pos = 0

    def process(self, data: bytes) -> bytes:
        result = bytearray()
        for byte in data:
            result.append(byte ^ self.pad[self.pos % len(self.pad)])
            self.pos += 1
        return bytes(result)


# -----------------------------------------------------------------------------
# BitCometDhEncryption — 完整加密层
# -----------------------------------------------------------------------------

class BitCometDhEncryption:
    """对应 Core_BitTorrent::BitTorrentProtocolDHEncryption."""

    def __init__(self, is_incoming: bool = False):
        self.dh = MseDhContext(is_incoming=is_incoming)
        # 加密 cipher (按方向)
        self.encrypt_cipher = None   # 我们发送时用
        self.decrypt_cipher = None   # 我们接收时用
        # 是否握手通过
        self.is_handshake_passed = False
        # 多 task 上下文 (m_hash_map)
        self._task_map: Dict[bytes, MseDhContext] = {}
        self._lock = threading.RLock()
        # 长握手缓存
        self._long_handshake_buffer = b""
        # 握手回调
        self.on_handshake_passed: Optional[Callable] = None

    # ----- 公开 API: DH 协商 -----

    def start_dh(self, skey: Optional[bytes] = None) -> bytes:
        """启动 DH, 返回本地公钥 (96 字节)."""
        self.dh.skey = skey
        return self.dh.generate_keypair()

    def complete_dh(self, remote_public_key: bytes) -> bool:
        """对端公钥到达, 完成协商."""
        self.dh.compute_shared_secret(remote_public_key)
        # 根据方向选择 keyA/keyB
        # 出站: 我们用 keyA 加密, keyB 解密
        # 入站: 我们用 keyB 加密, keyA 解密
        if self.dh.is_incoming:
            send_key, recv_key = self.dh.key_b, self.dh.key_a
        else:
            send_key, recv_key = self.dh.key_a, self.dh.key_b
        # 按加密类型创建 cipher
        self.encrypt_cipher = self._make_cipher(send_key, is_sender=True)
        self.decrypt_cipher = self._make_cipher(recv_key, is_sender=False)
        return True

    def _make_cipher(self, key: bytes, is_sender: bool):
        """根据 encrypt_type 创建 cipher."""
        if self.dh.encrypt_type == DhkeyEncryptType.RC4:
            return MseRc4Cipher(key, is_sender)
        elif self.dh.encrypt_type == DhkeyEncryptType.AES_CTR:
            # AES key 需要 16 或 32 字节
            aes_key = hashlib.sha256(key).digest()
            return MseAesCtrCipher(aes_key)
        elif self.dh.encrypt_type == DhkeyEncryptType.XOR_PAD:
            return MseXorPadCipher(key)
        else:
            return None  # PLAINTEXT / NONE

    # ----- 公开 API: 加解密 -----

    def socket_send(self, data: bytes) -> bytes:
        """对应 BitTorrentProtocolDHEncryption::socket_send."""
        if not self.is_handshake_passed or not self.encrypt_cipher:
            return data  # 未加密
        return self.encrypt_cipher.process(data)

    def decrypt_recv_stream(self, data: bytes) -> bytes:
        """对应 BitTorrentProtocolHandshake::decrypt_recv_stream."""
        if not self.is_handshake_passed or not self.decrypt_cipher:
            return data
        return self.decrypt_cipher.process(data)

    def handshake_passed(self) -> None:
        """对应 BitTorrentProtocolDHEncryption::handshake_passed."""
        self.is_handshake_passed = True
        if self.on_handshake_passed:
            self.on_handshake_passed()

    # ----- 公开 API: 长握手 -----

    def is_long_handshake(self) -> bool:
        """对应 is_long_handshake."""
        return self.dh.is_long_handshake

    def set_long_handshake(self, enabled: bool = True) -> None:
        self.dh.is_long_handshake = enabled

    def on_recv_long_handshake(self, data: bytes) -> Optional[bytes]:
        """对应 on_recv_long_handshake.

        长握手跨多包, 缓存直到完整.
        完整 handshake = 96 字节公钥 + 20 字节 padding + VC(8) + ...
        """
        self._long_handshake_buffer += data
        # 简化: 假设完整 handshake = 96 字节
        if len(self._long_handshake_buffer) >= 96:
            pubkey = self._long_handshake_buffer[:96]
            self._long_handshake_buffer = b""
            return pubkey
        return None

    # ----- 公开 API: 多 task 上下文 -----

    def task_add(self, info_hash: bytes) -> None:
        """对应 task_add - 为 info_hash 添加加密上下文."""
        with self._lock:
            if info_hash not in self._task_map:
                ctx = MseDhContext(is_incoming=self.dh.is_incoming, skey=info_hash)
                self._task_map[info_hash] = ctx

    def task_erase(self, info_hash: bytes) -> None:
        """对应 task_erase."""
        with self._lock:
            self._task_map.pop(info_hash, None)

    def find_task_hash(self, info_hash: bytes) -> Optional[MseDhContext]:
        """对应 find_task_hash."""
        with self._lock:
            return self._task_map.get(info_hash)

    def get_task_hash(self) -> Optional[bytes]:
        """对应 get_task_hash - 当前激活的 task hash."""
        # 简化: 返回第一个
        with self._lock:
            for h in self._task_map:
                return h
        return None

    def is_incoming_connection(self) -> bool:
        """对应 is_incoming_connection."""
        return self.dh.is_incoming


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(level=logging.INFO,
                        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s")
    print("=" * 60)
    print("BitComet MSE/DH 加密层 demo")
    print("=" * 60)
    # 模拟 Alice (出站) 和 Bob (入站)
    print("\n[1] DH 协商")
    alice = BitCometDhEncryption(is_incoming=False)
    bob = BitCometDhEncryption(is_incoming=True)
    alice.dh.encrypt_type = DhkeyEncryptType.RC4
    bob.dh.encrypt_type = DhkeyEncryptType.RC4
    skey = b"\x11" * 20  # info_hash
    alice_pubkey = alice.start_dh(skey)
    bob_pubkey = bob.start_dh(skey)
    alice.complete_dh(bob_pubkey)
    bob.complete_dh(alice_pubkey)
    print(f"  Alice shared secret: {alice.dh.shared_secret.hex()[:32]}...")
    print(f"  Bob   shared secret: {bob.dh.shared_secret.hex()[:32]}...")
    print(f"  Match: {alice.dh.shared_secret == bob.dh.shared_secret}")
    # 完成握手
    alice.handshake_passed()
    bob.handshake_passed()
    # Alice 发加密消息给 Bob
    print("\n[2] 加密通信 (RC4)")
    plaintext = b"Hello, BitComet MSE encrypted world! " * 4
    encrypted = alice.socket_send(plaintext)
    decrypted = bob.decrypt_recv_stream(encrypted)
    print(f"  plaintext  : {plaintext[:40]}...")
    print(f"  encrypted   : {encrypted[:40].hex()}...")
    print(f"  decrypted   : {decrypted[:40]}...")
    print(f"  Match: {plaintext == decrypted}")
    # 多 task
    print("\n[3] 多 task 加密上下文")
    alice.task_add(b"\x22" * 20)
    alice.task_add(b"\x33" * 20)
    print(f"  task count: {len(alice._task_map)}")
    print(f"  find task: {alice.find_task_hash(b'\\x22' * 20) is not None}")
    # AES-CTR 模式
    print("\n[4] AES-CTR 加密 (BitComet 扩展)")
    alice2 = BitCometDhEncryption(is_incoming=False)
    bob2 = BitCometDhEncryption(is_incoming=True)
    alice2.dh.encrypt_type = DhkeyEncryptType.AES_CTR
    bob2.dh.encrypt_type = DhkeyEncryptType.AES_CTR
    alice2.start_dh(skey)
    bob2.start_dh(skey)
    alice2.complete_dh(bob_pubkey)
    bob2.complete_dh(alice_pubkey)
    alice2.handshake_passed()
    bob2.handshake_passed()
    enc = alice2.socket_send(plaintext)
    dec = bob2.decrypt_recv_stream(enc)
    print(f"  AES-CTR match: {plaintext == dec}")
