"""
lt_seed_protocol.py — BitComet LT-Seeding 长期种子协议原型
=========================================================

逆向来源: BitComet `Core_BitTorrent::P2spLtSeedManager`
完整符号表 (逆向自 BitComet 2.21):
    P2spLtSeedManager::lt_query_add_one_file
    P2spLtSeedManager::lt_query_finished
    P2spLtSeedManager::lt_client_cancel
    P2spLtSeedManager::get_lt_seed
    P2spLtSeedManager::get_working_client_number_for_seed
    P2spLtSeedManager::prepare_http_ltseed_client_for_file
    P2spLtSeedManager::prepare_udp_ltseed_client_for_file
    P2spLtSeedManager::prepare_ltseed_clients_for_seed
    P2spLtSeedManager::update_ltseed_number_for_files
    P2spLtSeedManager::switch_to_other_file

数据结构 (从 STL 模板实例化反推):
    struct lt_file_t   { sha1_t file_hash; uint64_t file_size; path_t path; }
    struct lt_seed_t    { endpoint_t addr; uint8_t health; time_t last_seen; }

服务器端点 (来自 strings 提取):
    wss://repeater.bitcomet.com/ws/   (WebSocket 中继, NAT 穿透)
    passport-client.bitcomet.com:25476/25477  (HTTPS+SOAP 认证)

设计核心 (来自符号分析):
1. LT-Seeding = 把已下载完成的用户转为长期云端种子源 (P2P-CDN)
2. 双协议: HTTP LT-Seed (穿越 NAT) + UDP LT-Seed (低延迟)
3. 每个 lt_seed client 一次只服务一个 file, 用 SHA-1 作为 file_hash 索引
4. prepare_*_client_for_file 完成后, 上传/下载通道并发开启
5. switch_to_other_file: 当一个 file 下载完后, client 自动切换到下一个排队 file

加速价值 (针对 qBittorrent):
- qBittorrent 100% 依赖 tracker + DHT 找 peer
- 死种 (tracker 失效, DHT 没人) 时, LT-Seeding 是救场手段
- 等价于 BitComet 自建的"持久化 DHT + 云端 announce"

本模块提供 HTTP LT-Seed 协议的原型实现:
- LtSeedServer: 服务端 (你已完成下载, 把文件作为 LT-Seed 上传给其他客户端)
- LtSeedClient: 客户端 (你正在下载, 通过 LT-Seed 协议取分片)
- 协议格式参考 BitComet strings 中的 SOAP + REST 包格式

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import asyncio
import binascii
import hashlib
import json
import logging
import os
import socket
import struct
import time
import uuid
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Tuple

# 可选依赖
try:
    import aiohttp
except ImportError:
    aiohttp = None

LOG = logging.getLogger("ltseed")

# -----------------------------------------------------------------------------
# 协议常量 (来自 BitComet strings 反推)
# -----------------------------------------------------------------------------

LT_SEED_MAGIC = 0x4254434C      # "BTCL" (BitComet LT-Seed magic)
LT_SEED_VERSION = 1
LT_SEED_DEFAULT_PORT = 25432    # BitComet 内部 LT-Seed 默认端口

# 协议消息类型 (从 Core_BCSPClient SOAP 调用结构反推)
class MessageType:
    QUERY_SEED = 1          # client → server: 谁有这个 file_hash?
    QUERY_SEED_RESPONSE = 2 # server → client: lt_seed_t 列表
    REQUEST_PIECE = 3       # client → seed: 给我 piece N
    PIECE_DATA = 4          # seed → client: piece 数据
    ANNOUNCE_FILE = 5       # seed → server: 我有这个 file
    UNANNOUNCE_FILE = 6     # seed → server: 我不再提供这个 file
    HEARTBEAT = 7           # 双向: 保活
    SHUTDOWN = 8


# -----------------------------------------------------------------------------
# 数据结构 — 对应 lt_file_t / lt_seed_t
# -----------------------------------------------------------------------------

@dataclass
class LtFile:
    """对应 P2spLtSeedManager::lt_file_t."""
    file_hash: str           # 40-char SHA-1 hex
    file_size: int
    file_name: str
    piece_size: int = 1 << 16   # 64 KiB (LT-Seed 用较小分片)


@dataclass
class LtSeed:
    """对应 P2spLtSeedManager::lt_seed_t."""
    endpoint: Tuple[str, int]   # (host, port)
    file_hash: str
    is_alive: bool = True
    health: int = 100           # 0-100, 越高越快
    last_seen: float = 0.0
    avg_speed_bps: int = 0


# -----------------------------------------------------------------------------
# 协议封包 — 仿照 BitComet 自定义二进制协议
# -----------------------------------------------------------------------------

class ProtocolError(Exception):
    pass


def pack_message(msg_type: int, payload: bytes = b"") -> bytes:
    """封包: magic(4) | version(1) | msg_type(1) | payload_len(4) | payload."""
    if len(payload) > 0xFFFFFFFF:
        raise ProtocolError("payload too large")
    return struct.pack(">IBBI", LT_SEED_MAGIC, LT_SEED_VERSION,
                       msg_type, len(payload)) + payload


def unpack_message(buf: bytes) -> Tuple[int, bytes]:
    if len(buf) < 10:
        raise ProtocolError("buffer too short")
    magic, ver, mtype, plen = struct.unpack(">IBBI", buf[:10])
    if magic != LT_SEED_MAGIC:
        raise ProtocolError(f"bad magic: {magic:08x}")
    if ver != LT_SEED_VERSION:
        raise ProtocolError(f"unsupported version: {ver}")
    if len(buf) < 10 + plen:
        raise ProtocolError("incomplete payload")
    return mtype, buf[10:10 + plen]


def encode_query_seed(file_hash: str) -> bytes:
    """QUERY_SEED: payload = file_hash(40 hex chars)."""
    if len(file_hash) != 40:
        raise ValueError("file_hash must be 40-char SHA-1 hex")
    return pack_message(MessageType.QUERY_SEED, file_hash.encode())


def encode_query_seed_response(seeds: List[LtSeed]) -> bytes:
    """QUERY_SEED_RESPONSE: count(2) + [(ip4(4) + port(2) + health(1)) * count]."""
    payload = struct.pack(">H", len(seeds))
    for s in seeds:
        ip = socket.inet_aton(s.endpoint[0]) if ":" not in s.endpoint[0] else b"\x00\x00\x00\x00"
        port = s.endpoint[1]
        payload += ip + struct.pack(">HB", port, s.health)
    return pack_message(MessageType.QUERY_SEED_RESPONSE, payload)


def decode_query_seed_response(payload: bytes) -> List[LtSeed]:
    seeds = []
    if len(payload) < 2:
        return seeds
    count = struct.unpack(">H", payload[:2])[0]
    pos = 2
    for _ in range(count):
        if pos + 7 > len(payload):
            break
        ip = socket.inet_ntoa(payload[pos:pos+4])
        port, health = struct.unpack(">HB", payload[pos+4:pos+7])
        pos += 7
        seeds.append(LtSeed(
            endpoint=(ip, port), file_hash="",
            health=health, last_seen=time.time()
        ))
    return seeds


def encode_request_piece(file_hash: str, piece_index: int) -> bytes:
    """REQUEST_PIECE: file_hash(40) + piece_index(4)."""
    return pack_message(MessageType.REQUEST_PIECE,
                        file_hash.encode() + struct.pack(">I", piece_index))


def encode_piece_data(file_hash: str, piece_index: int, data: bytes) -> bytes:
    """PIECE_DATA: file_hash(40) + piece_index(4) + data_len(4) + data."""
    return pack_message(MessageType.PIECE_DATA,
                        file_hash.encode() + struct.pack(">II", piece_index, len(data)) + data)


def decode_piece_data(payload: bytes) -> Tuple[str, int, bytes]:
    if len(payload) < 48:
        raise ProtocolError("piece_data payload too short")
    file_hash = payload[:40].decode()
    piece_index, data_len = struct.unpack(">II", payload[40:48])
    data = payload[48:48 + data_len]
    if len(data) != data_len:
        raise ProtocolError(f"data length mismatch: expected {data_len}, got {len(data)}")
    return file_hash, piece_index, data


def encode_announce_file(file: LtFile, port: int) -> bytes:
    """ANNOUNCE_FILE: port(2) + file_hash(40) + file_size(8) + name_len(2) + name."""
    name_bytes = file.file_name.encode()
    return pack_message(MessageType.ANNOUNCE_FILE,
                        struct.pack(">H", port) + file.file_hash.encode() +
                        struct.pack(">QH", file.file_size, len(name_bytes)) + name_bytes)


# -----------------------------------------------------------------------------
# 文件 SHA-1 计算 (BitComet 兼容)
# -----------------------------------------------------------------------------

def compute_file_sha1(path: str) -> str:
    """对应 DownloadManager::calc_filehash_and_submit.

    BitComet LT-Seed 用整个文件的 SHA-1 (不是 BT 的 piece SHA-1).
    """
    sha = hashlib.sha1()
    with open(path, "rb") as f:
        while True:
            chunk = f.read(1 << 20)
            if not chunk:
                break
            sha.update(chunk)
    return sha.hexdigest()


# -----------------------------------------------------------------------------
# LtSeedServer — 你已完成下载, 把文件作为种子源暴露
# -----------------------------------------------------------------------------

class LtSeedServer:
    """LT-Seed 服务端: 暴露已下载文件, 接受 piece 请求.

    对应 P2spLtSeedManager::prepare_ltseed_clients_for_seed 服务端部分.
    """

    def __init__(self, listen_host: str = "0.0.0.0",
                 listen_port: int = LT_SEED_DEFAULT_PORT):
        self.listen_host = listen_host
        self.listen_port = listen_port
        # file_hash -> (file_path, file_size, piece_size)
        self.files: Dict[str, Tuple[str, int, int]] = {}
        self._server: Optional[asyncio.AbstractServer] = None
        self.stats = {"bytes_served": 0, "pieces_served": 0,
                       "connections": 0, "start_time": 0}

    def add_file(self, path: str, piece_size: int = 1 << 16) -> str:
        """添加文件并返回其 SHA-1 hash."""
        size = os.path.getsize(path)
        sha = compute_file_sha1(path)
        self.files[sha] = (path, size, piece_size)
        LOG.info("added LT-Seed file: %s -> %s (%d bytes)", path, sha, size)
        return sha

    def remove_file(self, file_hash: str) -> bool:
        return self.files.pop(file_hash, None) is not None

    async def start(self) -> None:
        self._server = await asyncio.start_server(
            self._handle_client, self.listen_host, self.listen_port
        )
        self.stats["start_time"] = time.time()
        LOG.info("LT-Seed server listening on %s:%d", self.listen_host, self.listen_port)

    async def stop(self) -> None:
        if self._server:
            self._server.close()
            await self._server.wait_closed()

    async def _handle_client(self, reader: asyncio.StreamReader,
                              writer: asyncio.StreamWriter) -> None:
        peer = writer.get_extra_info("peername")
        LOG.debug("LT-Seed connection from %s", peer)
        self.stats["connections"] += 1
        try:
            while True:
                # 读 header (10 bytes)
                header = await reader.readexactly(10)
                magic, ver, mtype, plen = struct.unpack(">IBBI", header)
                if magic != LT_SEED_MAGIC:
                    LOG.warning("bad magic from %s", peer)
                    return
                payload = await reader.readexactly(plen) if plen else b""
                await self._dispatch(mtype, payload, writer)
        except asyncio.IncompleteReadError:
            pass
        except Exception as e:
            LOG.warning("client %s error: %s", peer, e)
        finally:
            writer.close()

    async def _dispatch(self, mtype: int, payload: bytes,
                        writer: asyncio.StreamWriter) -> None:
        if mtype == MessageType.QUERY_SEED:
            file_hash = payload.decode()
            # 该文件是否存在?
            if file_hash in self.files:
                # 自己作为 seed 回报 (health=100, 用本机端口)
                seed = LtSeed(
                    endpoint=(self.listen_host, self.listen_port),
                    file_hash=file_hash, health=100,
                )
                resp = encode_query_seed_response([seed])
            else:
                resp = encode_query_seed_response([])
            writer.write(resp)
            await writer.drain()
        elif mtype == MessageType.REQUEST_PIECE:
            if len(payload) < 44:
                return
            file_hash = payload[:40].decode()
            piece_index = struct.unpack(">I", payload[40:44])[0]
            data = self._read_piece(file_hash, piece_index)
            if data is None:
                return
            writer.write(encode_piece_data(file_hash, piece_index, data))
            await writer.drain()
            self.stats["pieces_served"] += 1
            self.stats["bytes_served"] += len(data)
        elif mtype == MessageType.HEARTBEAT:
            writer.write(pack_message(MessageType.HEARTBEAT))
            await writer.drain()

    def _read_piece(self, file_hash: str, piece_index: int) -> Optional[bytes]:
        if file_hash not in self.files:
            return None
        path, size, psize = self.files[file_hash]
        offset = piece_index * psize
        if offset >= size:
            return None
        length = min(psize, size - offset)
        with open(path, "rb") as f:
            f.seek(offset)
            return f.read(length)


# -----------------------------------------------------------------------------
# LtSeedClient — 你在下载, 通过 LT-Seed 协议取分片
# -----------------------------------------------------------------------------

class LtSeedClient:
    """LT-Seed 客户端: 找 LT-Seed → 取分片 → 写入本地文件.

    对应 P2spLtSeedManager::prepare_udp_ltseed_client_for_file (UDP 版本见 _udp.py).
    本类是 HTTP 版本: prepare_http_ltseed_client_for_file.
    """

    def __init__(self, seed_servers: List[Tuple[str, int]]):
        """
        Args:
            seed_servers: 已知 LT-Seed 服务器列表 (来自 BitComet 云端 announce)
        """
        self.seed_servers = seed_servers
        # file_hash -> [LtSeed]  本地缓存
        self.seed_cache: Dict[str, List[LtSeed]] = {}
        self.timeout = 10.0

    async def query_seeds(self, file_hash: str) -> List[LtSeed]:
        """对应 P2spLtSeedManager::lt_query_add_one_file."""
        if file_hash in self.seed_cache:
            return self.seed_cache[file_hash]

        all_seeds: List[LtSeed] = []
        for host, port in self.seed_servers:
            try:
                reader, writer = await asyncio.open_connection(host, port)
                try:
                    writer.write(encode_query_seed(file_hash))
                    await writer.drain()
                    header = await asyncio.wait_for(reader.readexactly(10), self.timeout)
                    _, _, mtype, plen = struct.unpack(">IBBI", header)
                    payload = await asyncio.wait_for(reader.readexactly(plen), self.timeout) if plen else b""
                    if mtype == MessageType.QUERY_SEED_RESPONSE:
                        seeds = decode_query_seed_response(payload)
                        for s in seeds:
                            s.file_hash = file_hash
                        all_seeds.extend(seeds)
                finally:
                    writer.close()
            except Exception as e:
                LOG.debug("query %s:%d failed: %s", host, port, e)

        self.seed_cache[file_hash] = all_seeds
        LOG.info("found %d LT-Seeds for %s", len(all_seeds), file_hash[:16])
        return all_seeds

    async def fetch_piece(self, file_hash: str, piece_index: int,
                          seeds: Optional[List[LtSeed]] = None) -> Optional[bytes]:
        """对应 P2spLtSeedManager::get_lt_seed + REQUEST_PIECE.

        多 seed 轮询, 失败自动切换到下一个 (对应 switch_to_other_file).
        """
        if seeds is None:
            seeds = await self.query_seeds(file_hash)
        if not seeds:
            return None

        for seed in seeds:
            if not seed.is_alive:
                continue
            try:
                reader, writer = await asyncio.open_connection(*seed.endpoint)
                try:
                    writer.write(encode_request_piece(file_hash, piece_index))
                    await writer.drain()
                    header = await asyncio.wait_for(reader.readexactly(10), self.timeout)
                    _, _, mtype, plen = struct.unpack(">IBBI", header)
                    payload = await asyncio.wait_for(
                        reader.readexactly(plen), self.timeout
                    ) if plen else b""
                    if mtype == MessageType.PIECE_DATA:
                        _, _, data = decode_piece_data(payload)
                        return data
                finally:
                    writer.close()
            except Exception as e:
                LOG.debug("seed %s failed: %s, switching", seed.endpoint, e)
                seed.is_alive = False
                continue
        return None


# -----------------------------------------------------------------------------
# 中央协调器 — 模拟 BitComet 云端 announce 服务器
# -----------------------------------------------------------------------------

class LtSeedCoordinator:
    """简化版中央协调服务器: 跟踪哪些 client 持有哪些 file.

    实际 BitComet 通过 passport-client.bitcomet.com + WebSocket repeater 实现.
    """

    def __init__(self):
        # file_hash -> {endpoint: LtSeed}
        self.registry: Dict[str, Dict[Tuple[str, int], LtSeed]] = {}

    def register(self, file_hash: str, seed: LtSeed) -> None:
        self.registry.setdefault(file_hash, {})[seed.endpoint] = seed
        LOG.info("registered seed %s for %s", seed.endpoint, file_hash[:16])

    def unregister(self, file_hash: str, endpoint: Tuple[str, int]) -> None:
        if file_hash in self.registry:
            self.registry[file_hash].pop(endpoint, None)

    def lookup(self, file_hash: str) -> List[LtSeed]:
        return list(self.registry.get(file_hash, {}).values())


# -----------------------------------------------------------------------------
# CLI 演示
# -----------------------------------------------------------------------------

async def _demo_server(path: str, port: int) -> None:
    server = LtSeedServer(listen_port=port)
    file_hash = server.add_file(path)
    await server.start()
    print(f"LT-Seed server started on port {port}, file_hash = {file_hash}")
    print("Press Ctrl+C to stop...")
    try:
        while True:
            await asyncio.sleep(1)
            print(f"  served {server.stats['pieces_served']} pieces "
                  f"({server.stats['bytes_served']:,} bytes)")
    except KeyboardInterrupt:
        await server.stop()


if __name__ == "__main__":
    import argparse
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )
    ap = argparse.ArgumentParser(description="BitComet LT-Seed protocol prototype")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p_server = sub.add_parser("serve", help="start LT-Seed server for a file")
    p_server.add_argument("file")
    p_server.add_argument("--port", type=int, default=LT_SEED_DEFAULT_PORT)

    args = ap.parse_args()
    if args.cmd == "serve":
        asyncio.run(_demo_server(args.file, args.port))
