"""
repeater_ws_protocol.py — BitComet WebSocket Repeater NAT 穿透协议
================================================================

逆向来源: Core_RemoteAccess::RemoteAccessRepeater + RemoteAccessHTTP
关键符号:
    Core_RemoteAccess::RemoteAccessRepeater
    Core_RemoteAccess::repeater_error_enum
    Core_RemoteAccess::repeater_status_enum
    Core_RemoteAccess::RemoteAccessHTTP::need_auth_info
    Core_RemoteAccess::RemoteAccessHTTP::on_http_request
    Core_RemoteAccess::RemoteAccessHTTP::redirect_to_https
    Core_RemoteAccess::RemoteAccessHTTP::send_home_page
    Core_RemoteAccess::RemoteAccessHTTP::send_tasklist_xml
    Core_RemoteAccess::RemoteAccessHTTP::send_tasklist_rss
    Core_RemoteAccess::RemoteAccessHTTP::send_tasklist_page
    Core_RemoteAccess::RemoteAccessHTTP::send_root_page
    Core_RemoteAccess::RemoteAccessHTTP::get_task_log_table
    Core_RemoteAccess::RemoteAccessHTTP::on_post_timeout
    Core_RemoteAccess::RemoteAccessVipApi
    Core_RemoteAccess::vip_user_token_t

确认 URL (来自 strings):
    wss://repeater.bitcomet.com/ws/         ← WebSocket 中继服务器
    passport-client.bitcomet.com:25476/25477 ← CometID 认证

API 端点 (来自 strings, /api/webui/*):
    /api/webui/login
    /api/webui/ip_verify
    /api/webui/action
    /api/https_cert/get

设计核心:
1. BitComet 把 NAT 穿透设计成 WebSocket 中继 (而非标准 STUN/TURN)
   - 优点: WebSocket 走 443 端口, 几乎不会被防火墙拦截
   - 优点: 不需要单独的 TURN 服务器, repeater 即服务端
   - 缺点: 中继延迟比直连高 (~50-100ms RTT 增加)
2. 三种打洞模式 (从符号 get_hole_punch_mode 推断):
   - "direct": 直接连 (NAT 友好)
   - "introduce": 找 introducer 中转
   - "relay": 走 WebSocket repeater (兜底)
3. CometID 认证 + VIP token:
   - 普通用户: 中继带宽限制
   - VIP 用户 (vip_user_token_t): 优先中继 + 更高带宽
4. WebUI 远程访问: RemoteAccessHTTP 提供浏览器访问, 不需客户端
5. ip_verify: 远程访问前验证客户端 IP (防滥用)

加速价值 (针对 qBittorrent):
- qBittorrent 仅靠 UPnP/STUN, 在对称 NAT 后完全无法远程访问
- WebSocket repeater 是兜底方案, 保证可达性
- 自建下载器可用此协议:
  a) 手机端远程添加任务
  b) 局域网外访问 WebUI
  c) 多设备同步

本模块实现:
- RepeaterClient: WebSocket 中继客户端
- RepeaterMessage: 消息协议 (4 种 type)
- NatPunchOrchestrator: 三种打洞模式协调器
- VipToken: CometID VIP token 数据结构

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import time
import uuid
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Dict, List, Optional, Tuple

LOG = logging.getLogger("repeater")


# -----------------------------------------------------------------------------
# 枚举
# -----------------------------------------------------------------------------

class RepeaterStatus(IntEnum):
    """对应 Core_RemoteAccess::repeater_status_enum."""
    DISCONNECTED = 0
    CONNECTING = 1
    AUTHENTICATING = 2
    CONNECTED = 3
    RELAYING = 4           # 正在中继数据
    ERROR = 5


class RepeaterError(IntEnum):
    """对应 Core_RemoteAccess::repeater_error_enum."""
    NONE = 0
    AUTH_FAILED = 1         # CometID 认证失败
    NOT_VIP = 2             # 非 VIP, 中继被限速
    RATE_LIMITED = 3         # 超出中继带宽限制
    SESSION_EXPIRED = 4      # token 过期
    TARGET_OFFLINE = 5       # 目标客户端离线
    TARGET_REFUSED = 6        # 目标拒绝中继
    NETWORK_ERROR = 7         # 网络错误
    PROTOCOL_ERROR = 8        # 协议错误
    INTERNAL_ERROR = 9


class RepeaterMsgType(IntEnum):
    """WebSocket 消息类型 (从 RemoteAccessRepeater 反推)."""
    AUTH = 1                # client → repeater: 认证
    AUTH_RESPONSE = 2        # repeater → client: 认证响应
    PUNCH_REQUEST = 3        # client A → repeater → B: 请求打洞
    PUNCH_RESPONSE = 4        # client B → repeater → A: 打洞响应
    RELAY_DATA = 5           # 双向: 中继数据
    RELAY_ACK = 6            # 数据 ack
    HEARTBEAT = 7             # 保活
    DISCONNECT = 8            # 主动断开


class HolePunchMode(IntEnum):
    """对应 BitTorrentPeerPool::get_hole_punch_mode."""
    DIRECT = 0          # 直连 (对端公网 IP)
    INTRODUCE = 1        # 通过 introducer 中转
    RELAY = 2            # 走 WebSocket repeater


# -----------------------------------------------------------------------------
# 数据结构
# -----------------------------------------------------------------------------

@dataclass
class VipToken:
    """对应 Core_RemoteAccess::vip_user_token_t.

    CometID VIP 用户的认证 token.
    """
    user_id: int
    token: str               # 64-char hex
    expires_at: float
    vip_level: int = 0       # 0=普通, 1=VIP, 2=VIP+
    bandwidth_limit_bps: int = 0   # 0 = 不限

    def is_expired(self) -> bool:
        return time.time() > self.expires_at

    def is_vip(self) -> bool:
        return self.vip_level > 0


@dataclass
class RepeaterMessage:
    """WebSocket 消息封装."""
    msg_type: RepeaterMsgType
    payload: bytes = b""
    seq: int = 0
    target_session: Optional[str] = None  # 目标客户端 session_id
    src_session: Optional[str] = None    # 源客户端 session_id
    error: RepeaterError = RepeaterError.NONE


# -----------------------------------------------------------------------------
# 编码器
# -----------------------------------------------------------------------------

class RepeaterProtocol:
    """消息编解码.

    帧格式:
        magic(4) "BCRP"  |  version(1)  |  msg_type(1)  |  flags(1)
        seq(4)  |  target_session_len(2)  |  target_session(...)
        src_session_len(2)  |  src_session(...)
        error(1)  |  payload_len(4)  |  payload(...)
    """

    MAGIC = b"BCRP"   # BitComet Repeater Protocol
    VERSION = 1

    @staticmethod
    def encode(msg: RepeaterMessage) -> bytes:
        buf = bytearray()
        buf += RepeaterProtocol.MAGIC
        buf += bytes([RepeaterProtocol.VERSION, int(msg.msg_type), 0])
        buf += msg.seq.to_bytes(4, "big")
        # target session
        if msg.target_session:
            ts = msg.target_session.encode("utf-8")
            buf += len(ts).to_bytes(2, "big") + ts
        else:
            buf += b"\x00\x00"
        # src session
        if msg.src_session:
            ss = msg.src_session.encode("utf-8")
            buf += len(ss).to_bytes(2, "big") + ss
        else:
            buf += b"\x00\x00"
        # error + payload
        buf += bytes([int(msg.error)])
        buf += len(msg.payload).to_bytes(4, "big") + msg.payload
        return bytes(buf)

    @staticmethod
    def decode(data: bytes) -> Optional[RepeaterMessage]:
        if len(data) < 14 or data[:4] != RepeaterProtocol.MAGIC:
            return None
        version, msg_type, flags = data[4], data[5], data[6]
        if version != RepeaterProtocol.VERSION:
            return None
        seq = int.from_bytes(data[7:11], "big")
        pos = 11
        # target session
        ts_len = int.from_bytes(data[pos:pos+2], "big"); pos += 2
        target = data[pos:pos+ts_len].decode("utf-8", errors="replace") if ts_len else None
        pos += ts_len
        # src session
        ss_len = int.from_bytes(data[pos:pos+2], "big"); pos += 2
        src = data[pos:pos+ss_len].decode("utf-8", errors="replace") if ss_len else None
        pos += ss_len
        # error + payload
        error = RepeaterError(data[pos]) if pos < len(data) else RepeaterError.NONE
        pos += 1
        if pos + 4 > len(data):
            return None
        payload_len = int.from_bytes(data[pos:pos+4], "big"); pos += 4
        if pos + payload_len > len(data):
            return None
        payload = data[pos:pos+payload_len]
        return RepeaterMessage(
            msg_type=RepeaterMsgType(msg_type), payload=payload, seq=seq,
            target_session=target, src_session=src, error=error,
        )


# -----------------------------------------------------------------------------
# RepeaterClient — WebSocket 中继客户端
# -----------------------------------------------------------------------------

class RepeaterClient:
    """WebSocket 中继客户端.

    用法:
        client = RepeaterClient(
            repeater_url="wss://repeater.bitcomet.com/ws/",
            vip_token=VipToken(...),
        )
        await client.connect()
        # 注册本机会话
        await client.register("my_session_id")
        # 发数据给另一个客户端
        await client.send_relay("target_session", b"hello")
        # 接收中继数据
        client.on_relay_data = lambda src, data: print(f"from {src}: {data}")
    """

    def __init__(self, repeater_url: str = "wss://repeater.bitcomet.com/ws/",
                 vip_token: Optional[VipToken] = None,
                 session_id: Optional[str] = None):
        self.url = repeater_url
        self.vip_token = vip_token
        self.session_id = session_id or str(uuid.uuid4())
        self.status = RepeaterStatus.DISCONNECTED
        # WebSocket 连接
        self._ws = None
        # 序号
        self._seq = 0
        # 接收回调
        self.on_relay_data: Optional[callable] = None
        self.on_punch_request: Optional[callable] = None
        self.on_auth_response: Optional[callable] = None
        # 等待响应的 future
        self._pending: Dict[int, asyncio.Future] = {}

    async def connect(self) -> bool:
        """连接到 repeater 服务器."""
        try:
            import aiohttp
        except ImportError:
            LOG.error("aiohttp required for WebSocket repeater")
            return False
        self.status = RepeaterStatus.CONNECTING
        LOG.info("connecting to repeater: %s", self.url)
        try:
            self._ws = await aiohttp.ClientSession().ws_connect(self.url, timeout=15)
            self.status = RepeaterStatus.AUTHENTICATING
            # 启动接收循环
            asyncio.create_task(self._recv_loop())
            # 发送 AUTH
            auth_payload = json.dumps({
                "session_id": self.session_id,
                "user_id": self.vip_token.user_id if self.vip_token else 0,
                "token": self.vip_token.token if self.vip_token else "",
                "vip_level": self.vip_token.vip_level if self.vip_token else 0,
                "client_version": "1.0",  # 自研下载器版本
            }).encode("utf-8")
            auth_msg = RepeaterMessage(
                msg_type=RepeaterMsgType.AUTH,
                payload=auth_payload,
                seq=self._next_seq(),
                src_session=self.session_id,
            )
            await self._send(auth_msg)
            # 等待 AUTH_RESPONSE
            fut = asyncio.get_event_loop().create_future()
            self._pending[auth_msg.seq] = fut
            try:
                resp = await asyncio.wait_for(fut, timeout=10)
                if resp.error == RepeaterError.NONE:
                    self.status = RepeaterStatus.CONNECTED
                    LOG.info("authenticated, session=%s", self.session_id)
                    return True
                else:
                    LOG.error("auth failed: %s", resp.error.name)
                    self.status = RepeaterStatus.ERROR
                    return False
            except asyncio.TimeoutError:
                LOG.error("auth timeout")
                self.status = RepeaterStatus.ERROR
                return False
        except Exception as e:
            LOG.error("connect failed: %s", e)
            self.status = RepeaterStatus.ERROR
            return False

    async def disconnect(self) -> None:
        if self._ws:
            msg = RepeaterMessage(
                msg_type=RepeaterMsgType.DISCONNECT,
                src_session=self.session_id,
            )
            await self._send(msg)
            await self._ws.close()
            self._ws = None
        self.status = RepeaterStatus.DISCONNECTED

    async def send_relay(self, target_session: str, data: bytes) -> bool:
        """通过 repeater 中继数据给 target_session."""
        if self.status != RepeaterStatus.CONNECTED:
            return False
        msg = RepeaterMessage(
            msg_type=RepeaterMsgType.RELAY_DATA,
            payload=data,
            seq=self._next_seq(),
            target_session=target_session,
            src_session=self.session_id,
        )
        await self._send(msg)
        return True

    async def request_punch(self, target_session: str) -> Optional[RepeaterMessage]:
        """请求 repeater 协调打洞 (对应 PUNCH_REQUEST)."""
        if self.status != RepeaterStatus.CONNECTED:
            return None
        seq = self._next_seq()
        msg = RepeaterMessage(
            msg_type=RepeaterMsgType.PUNCH_REQUEST,
            payload=json.dumps({"target": target_session}).encode(),
            seq=seq,
            target_session=target_session,
            src_session=self.session_id,
        )
        await self._send(msg)
        fut = asyncio.get_event_loop().create_future()
        self._pending[seq] = fut
        try:
            return await asyncio.wait_for(fut, timeout=15)
        except asyncio.TimeoutError:
            LOG.warning("punch request timeout")
            return None

    # ----- 内部 -----

    def _next_seq(self) -> int:
        self._seq += 1
        return self._seq

    async def _send(self, msg: RepeaterMessage) -> None:
        if not self._ws:
            return
        await self._ws.send_bytes(RepeaterProtocol.encode(msg))

    async def _recv_loop(self) -> None:
        try:
            async for ws_msg in self._ws:
                if ws_msg.type != aiohttp.WSMsgType.BINARY if self._ws else False:
                    continue
                msg = RepeaterProtocol.decode(ws_msg.data)
                if not msg:
                    continue
                await self._handle_msg(msg)
        except Exception as e:
            LOG.error("recv loop ended: %s", e)
            self.status = RepeaterStatus.ERROR

    async def _handle_msg(self, msg: RepeaterMessage) -> None:
        # 完成 pending future
        if msg.seq in self._pending:
            fut = self._pending.pop(msg.seq)
            if not fut.done():
                fut.set_result(msg)
            return
        # 路由到回调
        if msg.msg_type == RepeaterMsgType.RELAY_DATA:
            if self.on_relay_data:
                self.on_relay_data(msg.src_session, msg.payload)
        elif msg.msg_type == RepeaterMsgType.PUNCH_REQUEST:
            if self.on_punch_request:
                # 自动响应: 接受打洞
                resp = RepeaterMessage(
                    msg_type=RepeaterMsgType.PUNCH_RESPONSE,
                    payload=json.dumps({"accept": True}).encode(),
                    seq=self._next_seq(),
                    target_session=msg.src_session,
                    src_session=self.session_id,
                )
                await self._send(resp)
                self.on_punch_request(msg.src_session)
        elif msg.msg_type == RepeaterMsgType.AUTH_RESPONSE:
            if self.on_auth_response:
                self.on_auth_response(msg)
        elif msg.msg_type == RepeaterMsgType.HEARTBEAT:
            await self._send(RepeaterMessage(
                msg_type=RepeaterMsgType.HEARTBEAT,
                src_session=self.session_id,
            ))


# -----------------------------------------------------------------------------
# NatPunchOrchestrator — 三模式协调器
# -----------------------------------------------------------------------------

class NatPunchOrchestrator:
    """对应 BitTorrentPeerPool::get_hole_punch_mode + find_introducer_for_peer.

    协调三种 NAT 穿透模式, 自动选择最优.
    """

    def __init__(self, repeater: Optional[RepeaterClient] = None):
        self.repeater = repeater
        # peer graph: peer → 已知 peers
        self._peer_graph: Dict[str, Set[str]] = {}

    def update_peer_graph(self, peer: str, their_peers: Set[str]) -> None:
        self._peer_graph[peer] = their_peers.copy()

    def decide_mode(self, target: str, target_is_public: bool = False) -> HolePunchMode:
        """决定用哪种打洞模式."""
        if target_is_public:
            return HolePunchMode.DIRECT
        # 找 introducer
        for introducer, peers in self._peer_graph.items():
            if target in peers:
                return HolePunchMode.INTRODUCE
        # 兜底: relay
        if self.repeater and self.repeater.status == RepeaterStatus.CONNECTED:
            return HolePunchMode.RELAY
        raise RuntimeError("no punch mode available (repeater offline + no introducer)")

    async def connect_via_mode(self, target: str,
                                mode: HolePunchMode) -> Optional[bytes]:
        """按选定模式打洞."""
        if mode == HolePunchMode.DIRECT:
            # 直连, 不通过 repeater
            return None
        elif mode == HolePunchMode.INTRODUCE:
            # 找 introducer, 让它中转 PUNCH_REQUEST
            for introducer, peers in self._peer_graph.items():
                if target in peers:
                    if self.repeater:
                        resp = await self.repeater.request_punch(target)
                        if resp and resp.error == RepeaterError.NONE:
                            return resp.payload
            return None
        elif mode == HolePunchMode.RELAY:
            if self.repeater:
                # 走 repeater 中继, 发个 RELAY_DATA 探测
                ok = await self.repeater.send_relay(target, b"punch_probe")
                return b"sent" if ok else None
        return None


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(level=logging.INFO,
                        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s")
    ap = argparse.ArgumentParser(description="WebSocket Repeater Protocol demo")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p_enc = sub.add_parser("encode", help="演示消息编码")
    p_punch = sub.add_parser("decide", help="演示打洞模式决策")
    p_punch.add_argument("--target", default="peer_xxx")
    p_punch.add_argument("--public", action="store_true")

    args = ap.parse_args()

    if args.cmd == "encode":
        # 演示 AUTH 消息编码
        msg = RepeaterMessage(
            msg_type=RepeaterMsgType.AUTH,
            payload=b'{"session_id":"abc","token":"xyz"}',
            seq=1, src_session="abc",
        )
        encoded = RepeaterProtocol.encode(msg)
        print(f"encoded {len(encoded)} bytes: {encoded[:30]!r}...")
        decoded = RepeaterProtocol.decode(encoded)
        print(f"decoded: type={decoded.msg_type.name} seq={decoded.seq} "
              f"src={decoded.src_session} payload_len={len(decoded.payload)}")
        print(f"payload: {decoded.payload.decode()}")

    elif args.cmd == "decide":
        orch = NatPunchOrchestrator(repeater=None)
        # 模拟 peer graph: A 知道 B, B 知道 C
        orch.update_peer_graph("peer_A", {"peer_B"})
        orch.update_peer_graph("peer_B", {"peer_C"})
        # 测试 C (私网)
        mode = orch.decide_mode(args.target, target_is_public=args.public)
        print(f"target={args.target} public={args.public} → mode={mode.name}")
