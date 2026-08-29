"""
lt_seed_cloud_client.py — BitComet LT-Seed 云端 announce 客户端
==========================================================

逆向来源: Core_P2SPClient + Core_BCSPClient (云端通信层)
关键符号:
    Core_P2SPClient::TorrentShareQueryWrapper::soap_succeed
    Core_P2SPClient::TorrentShareQueryWrapper::rest_succeed
    Core_P2SPClient::TorrentShareSubmitWrapper::submit_torrent_file
    Core_P2SPClient::TorrentShareSubmitWrapper::submit_torrent_content
    Core_P2SPClient::HTTPShareQueryWrapper::soap_succeed
    Core_P2SPClient::HTTPShareAnnounceWrapper::announce
    Core_BCSPClient::BCSPClient
    Core_BCSPClient::RestNameAccountLoginPassword
    Core_BCSPClient::RestNameAccountLoginToken
    Core_BCSPClient::RestNameAccountLoginPasswordResult
    Core_BCSPClient::RestNameAccountLoginTokenResult
    Core_BCSPClient::RestNameDeviceLogout
    Core_BCSPClient::RestNameDeviceLogoutResult
    Core_BCSPClient::RestNameScoreUpdate
    Core_BCSPClient::RestNameScoreUpdateResult
    Core_BCSPClient::RestNameSupporterUpdate
    Core_BCSPClient::RestNameSupporterUpdateResult
    Core_BCSPClient::RestNameSubscriberAndroidPay
    Core_BCSPClient::RestNameSubscriberAndroidPayResult
    Core_SOAPClient::REST_Package
    Core_SOAPClient::REST_Package::build(vector_buffer&)
    Core_SOAPClient::REST_Package::parse(string_view)
    Core_SOAPClient::REST_Package::header_length
    Core_SOAPClient::REST_Package::is_response_ok
    Core_SOAPClient::REST_Package::is_response_error
    Core_SOAPClient::soap_wrapper_t::rest_request(REST_Package, unsigned int)
    Core_SOAPClient::soap_async_operater_t::rest_succeed
    Core_SOAPClient::soap_sync_operater_t::rest_succeed
    Core_Common::AsyncTaskHelper<shared_ptr<REST_Package>>::task_info_t::invoke_finish_func
    Core_Common::AsyncTaskHelper<shared_ptr<REST_Package>>::task_info_t::invoke_thread_func

确认端点 (来自 strings):
    passport-client.bitcomet.com:25476  ← HTTP REST API
    passport-client.bitcomet.com:25477  ← 备用 (HTTPS)

确认 API 路径 (来自 /api/* strings):
    /api/cometid/query
    /api/cometid/sign_in
    /api/cometid/sign_out
    /api/device_token/get
    /api/config/bound_device/{remove,rename}
    /api/config/bound_devices/get
    /api/notifications/action
    /api/notifications/get

设计核心 (从符号分析):
1. BCSP (BitComet Service Protocol) = REST API over HTTP(S)
2. 双认证方式:
   - RestNameAccountLoginPassword: 用户名 + 密码
   - RestNameAccountLoginToken: 设备 token (长期有效)
3. RestNameScoreUpdate: 积分激励 (LT-Seed 上传贡献积分)
4. RestNameSupporterUpdate: 捐赠者状态更新
5. RestNameSubscriberAndroidPay: Android 端订阅支付
6. REST_Package: BitComet 自定义 REST 包装, 含 header + body
7. 双通道: SOAP (XML) + REST (JSON), 取决于接口

加速价值 (针对 qBittorrent):
- qBittorrent 无任何账户系统, 全本地运行
- 自建 LT-Seed 云端协调服务器需要这套认证协议
- 移植此模块后, 可以:
  a) 让用户登录自己的云端
  b) 上报本地 LT-Seed 文件 hash
  c) 查询其他客户端的 LT-Seed
  d) 累计积分激励长期 seed

本模块实现:
- BCSPClient: BitComet Service Protocol 客户端
- LTSeedCloudClient: 基于 BCSP 的 LT-Seed 云端协调
- 设备 token 持久化
- 积分上报 (ScoreUpdate)

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import os
import time
import uuid
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Dict, List, Optional

try:
    import aiohttp
except ImportError:
    aiohttp = None

LOG = logging.getLogger("ltseed_cloud")


# -----------------------------------------------------------------------------
# 枚举
# -----------------------------------------------------------------------------

class BcspAuthMethod(IntEnum):
    """对应 RestNameAccountLogin*."""
    PASSWORD = 0     # 用户名 + 密码
    TOKEN = 1         # 设备 token (长期)


class RestName(IntEnum):
    """对应 Core_BCSPClient::RestName* 枚举.

    每个 enum 值对应一个 REST API 端点.
    """
    ACCOUNT_LOGIN_PASSWORD = 0        # /api/cometid/sign_in
    ACCOUNT_LOGIN_TOKEN = 1            # /api/device_token/get
    DEVICE_LOGOUT = 2                  # /api/cometid/sign_out
    SCORE_UPDATE = 3                   # /api/score/update (推测)
    SUPPORTER_UPDATE = 4              # /api/supporter/update (推测)
    SUBSCRIBER_ANDROID_PAY = 5         # /api/android/googleplay/pay
    LT_SEED_QUERY = 10                 # /api/ltseed/query (推测)
    LT_SEED_SUBMIT = 11                # /api/ltseed/submit (推测)
    TORRENT_SHARE_QUERY = 20           # /api/torrent/share/query
    TORRENT_SHARE_SUBMIT = 21          # /api/torrent/share/submit
    HTTP_SHARE_QUERY = 30              # /api/http/share/query
    HTTP_SHARE_ANNOUNCE = 31           # /api/http/share/announce


# 端点路径映射 (从 strings 提取 + 推测)
REST_ENDPOINTS: Dict[RestName, str] = {
    RestName.ACCOUNT_LOGIN_PASSWORD: "/api/cometid/sign_in",
    RestName.ACCOUNT_LOGIN_TOKEN: "/api/device_token/get",
    RestName.DEVICE_LOGOUT: "/api/cometid/sign_out",
    RestName.SCORE_UPDATE: "/api/score/update",
    RestName.SUPPORTER_UPDATE: "/api/supporter/update",
    RestName.SUBSCRIBER_ANDROID_PAY: "/api/android/googleplay/pay",
    RestName.LT_SEED_QUERY: "/api/ltseed/query",
    RestName.LT_SEED_SUBMIT: "/api/ltseed/submit",
    RestName.TORRENT_SHARE_QUERY: "/api/torrent/share/query",
    RestName.TORRENT_SHARE_SUBMIT: "/api/torrent/share/submit",
    RestName.HTTP_SHARE_QUERY: "/api/http/share/query",
    RestName.HTTP_SHARE_ANNOUNCE: "/api/http/share/announce",
}


# -----------------------------------------------------------------------------
# REST_Package — 对应 Core_SOAPClient::REST_Package
# -----------------------------------------------------------------------------

@dataclass
class RESTPackage:
    """对应 Core_SOAPClient::REST_Package.

    BitComet 自定义 REST 包装:
    - header: 长度前缀 JSON (header_length + body_length)
    - body: JSON 或 binary
    """
    name: RestName
    request_id: str = field(default_factory=lambda: str(uuid.uuid4()))
    timestamp: int = field(default_factory=lambda: int(time.time()))
    # 鉴权信息
    auth_token: Optional[str] = None
    device_id: Optional[str] = None
    # 负载
    payload: Dict[str, Any] = field(default_factory=dict)
    # 响应字段
    is_ok: bool = False
    is_error: bool = False
    error_msg: Optional[str] = None

    def build(self) -> bytes:
        """对应 REST_Package::build(vector_buffer&)."""
        header = {
            "name": int(self.name),
            "request_id": self.request_id,
            "timestamp": self.timestamp,
            "auth_token": self.auth_token,
            "device_id": self.device_id,
        }
        header_json = json.dumps(header, separators=(",", ":")).encode("utf-8")
        body_json = json.dumps(self.payload, separators=(",", ":")).encode("utf-8")
        # 帧格式: header_len(4) + header_json + body_len(4) + body_json
        return (len(header_json).to_bytes(4, "big") + header_json +
                len(body_json).to_bytes(4, "big") + body_json)

    @classmethod
    def parse(cls, data: bytes) -> Optional["RESTPackage"]:
        """对应 REST_Package::parse(string_view)."""
        if len(data) < 8:
            return None
        header_len = int.from_bytes(data[:4], "big")
        if len(data) < 4 + header_len + 4:
            return None
        header_json = data[4:4+header_len].decode("utf-8", errors="replace")
        body_len_pos = 4 + header_len
        body_len = int.from_bytes(data[body_len_pos:body_len_pos+4], "big")
        body_pos = body_len_pos + 4
        if len(data) < body_pos + body_len:
            return None
        body_json = data[body_pos:body_pos+body_len].decode("utf-8", errors="replace")
        try:
            header = json.loads(header_json)
            body = json.loads(body_json)
        except json.JSONDecodeError as e:
            LOG.error("REST_Package parse failed: %s", e)
            return None
        pkg = cls(name=RestName(header.get("name", 0)))
        pkg.request_id = header.get("request_id", "")
        pkg.timestamp = header.get("timestamp", 0)
        pkg.auth_token = header.get("auth_token")
        pkg.device_id = header.get("device_id")
        pkg.payload = body
        pkg.is_ok = body.get("status") == "ok"
        pkg.is_error = body.get("status") == "error"
        pkg.error_msg = body.get("error_msg")
        return pkg

    def is_response_ok(self) -> bool:
        """对应 REST_Package::is_response_ok."""
        return self.is_ok

    def is_response_error(self) -> bool:
        """对应 REST_Package::is_response_error."""
        return self.is_error


# -----------------------------------------------------------------------------
# BCSPClient — BitComet Service Protocol 客户端
# -----------------------------------------------------------------------------

class BCSPClient:
    """对应 Core_BCSPClient::BCSPClient.

    用法:
        client = BCSPClient(
            server_host="passport-client.bitcomet.com",
            server_port=25476,
        )
        await client.connect()
        # 1. 密码登录
        await client.login_password("user", "pass")
        # 2. 或 token 登录
        await client.login_token("device_token_xxx")
        # 3. 调用接口
        result = await client.rest_request(RestName.LT_SEED_QUERY, {"info_hash": "abc"})
        # 4. 注销
        await client.logout()
    """

    def __init__(self, server_host: str = "passport-client.bitcomet.com",
                 server_port: int = 25476, use_https: bool = False,
                 device_id: Optional[str] = None):
        self.host = server_host
        self.port = server_port
        self.use_https = use_https
        self.device_id = device_id or str(uuid.uuid4())
        # 鉴权状态
        self.auth_token: Optional[str] = None
        self.user_id: Optional[int] = None
        self.is_authenticated = False
        # HTTP session
        self._session = None
        # 异步任务队列 (对应 AsyncTaskHelper)
        self._pending: Dict[str, asyncio.Future] = {}

    async def connect(self) -> bool:
        if aiohttp is None:
            LOG.error("aiohttp required")
            return False
        base = f"{'https' if self.use_https else 'http'}://{self.host}:{self.port}"
        self._session = aiohttp.ClientSession(base_url=base, timeout=aiohttp.ClientTimeout(total=30))
        LOG.info("BCSP connected to %s", base)
        return True

    async def close(self) -> None:
        if self._session:
            await self._session.close()

    # ----- 公开 API: 鉴权 -----

    async def login_password(self, username: str, password: str) -> bool:
        """对应 RestNameAccountLoginPassword."""
        # 密码 SHA-256 (避免明文传输, 实际 BitComet 可能用更复杂加密)
        password_hash = hashlib.sha256(password.encode()).hexdigest()
        pkg = RESTPackage(name=RestName.ACCOUNT_LOGIN_PASSWORD)
        pkg.payload = {
            "username": username,
            "password_hash": password_hash,
            "device_id": self.device_id,
        }
        resp = await self._rest_request_internal(pkg)
        if resp and resp.is_response_ok():
            self.auth_token = resp.payload.get("token")
            self.user_id = resp.payload.get("user_id")
            self.is_authenticated = True
            LOG.info("login success, user_id=%s", self.user_id)
            return True
        LOG.error("login failed: %s", resp.error_msg if resp else "no response")
        return False

    async def login_token(self, device_token: str) -> bool:
        """对应 RestNameAccountLoginToken."""
        pkg = RESTPackage(name=RestName.ACCOUNT_LOGIN_TOKEN)
        pkg.payload = {
            "device_token": device_token,
            "device_id": self.device_id,
        }
        resp = await self._rest_request_internal(pkg)
        if resp and resp.is_response_ok():
            self.auth_token = resp.payload.get("token")
            self.user_id = resp.payload.get("user_id")
            self.is_authenticated = True
            LOG.info("token login success, user_id=%s", self.user_id)
            return True
        return False

    async def logout(self) -> bool:
        """对应 RestNameDeviceLogout."""
        if not self.is_authenticated:
            return True
        pkg = RESTPackage(name=RestName.DEVICE_LOGOUT, auth_token=self.auth_token,
                          device_id=self.device_id)
        resp = await self._rest_request_internal(pkg)
        self.auth_token = None
        self.user_id = None
        self.is_authenticated = False
        return resp is not None and resp.is_response_ok()

    # ----- 公开 API: 通用 REST 请求 -----

    async def rest_request(self, name: RestName,
                            payload: Optional[Dict] = None) -> Optional[RESTPackage]:
        """对应 soap_wrapper_t::rest_request(REST_Package, unsigned int)."""
        if not self.is_authenticated and name not in (
            RestName.ACCOUNT_LOGIN_PASSWORD, RestName.ACCOUNT_LOGIN_TOKEN
        ):
            LOG.error("not authenticated")
            return None
        pkg = RESTPackage(
            name=name,
            payload=payload or {},
            auth_token=self.auth_token,
            device_id=self.device_id,
        )
        return await self._rest_request_internal(pkg)

    # ----- 内部 -----

    async def _rest_request_internal(self, pkg: RESTPackage) -> Optional[RESTPackage]:
        if not self._session:
            await self.connect()
        endpoint = REST_ENDPOINTS.get(pkg.name)
        if not endpoint:
            LOG.error("unknown rest name: %s", pkg.name)
            return None
        try:
            body = pkg.build()
            async with self._session.post(endpoint, data=body) as resp:
                if resp.status != 200:
                    LOG.error("HTTP %d for %s", resp.status, endpoint)
                    return None
                data = await resp.read()
            return RESTPackage.parse(data)
        except Exception as e:
            LOG.error("rest_request %s failed: %s", endpoint, e)
            return None


# -----------------------------------------------------------------------------
# LTSeedCloudClient — LT-Seed 云端协调
# -----------------------------------------------------------------------------

class LTSeedCloudClient:
    """基于 BCSP 的 LT-Seed 云端协调客户端.

    功能:
    1. submit_ltseed: 本地有文件, 上报 file_hash + endpoint
    2. query_ltseed: 查询谁有该 file_hash
    3. unsubmit_ltseed: 文件不再提供
    4. update_score: 上报本地上传字节数, 累计积分
    """

    def __init__(self, bcsp: BCSPClient, my_listen_port: int = 25432):
        self.bcsp = bcsp
        self.my_port = my_listen_port
        # 本地已上报的 file_hash → endpoint (用于断线重连)
        self._announced: Dict[str, Dict] = {}

    async def submit_ltseed(self, file_hash: str, file_size: int,
                             file_name: str) -> bool:
        """对应 TorrentShareSubmitWrapper::submit_torrent_file."""
        # 获取本机公网 IP (用 ipify)
        my_ip = await self._get_public_ip()
        if not my_ip:
            return False
        endpoint = {"ip": my_ip, "port": self.my_listen_port}
        pkg_payload = {
            "file_hash": file_hash,
            "file_size": file_size,
            "file_name": file_name,
            "endpoint": endpoint,
            "device_id": self.bcsp.device_id,
        }
        resp = await self.bcsp.rest_request(RestName.LT_SEED_SUBMIT, pkg_payload)
        if resp and resp.is_response_ok():
            self._announced[file_hash] = pkg_payload
            LOG.info("announced LT-Seed for %s at %s:%d",
                     file_hash[:16], my_ip, self.my_port)
            return True
        return False

    async def query_ltseed(self, file_hash: str) -> List[Dict]:
        """对应 TorrentShareQueryWrapper::rest_succeed."""
        resp = await self.bcsp.rest_request(
            RestName.LT_SEED_QUERY, {"file_hash": file_hash}
        )
        if resp and resp.is_response_ok():
            seeds = resp.payload.get("seeds", [])
            LOG.info("found %d LT-Seeds for %s", len(seeds), file_hash[:16])
            return seeds
        return []

    async def unsubmit_ltseed(self, file_hash: str) -> bool:
        """文件不再提供."""
        resp = await self.bcsp.rest_request(
            RestName.LT_SEED_SUBMIT,
            {"file_hash": file_hash, "action": "remove"}
        )
        if resp and resp.is_response_ok():
            self._announced.pop(file_hash, None)
            return True
        return False

    async def update_score(self, bytes_uploaded: int) -> bool:
        """对应 RestNameScoreUpdate.

        上报本地上传字节数, 累计 LT-Seed 积分.
        """
        resp = await self.bcsp.rest_request(
            RestName.SCORE_UPDATE,
            {"bytes_uploaded": bytes_uploaded, "device_id": self.bcsp.device_id}
        )
        if resp and resp.is_response_ok():
            score = resp.payload.get("new_score", 0)
            LOG.info("score updated: +%d bytes, total score=%d",
                     bytes_uploaded, score)
            return True
        return False

    async def _get_public_ip(self) -> Optional[str]:
        if aiohttp is None:
            return None
        try:
            async with aiohttp.ClientSession() as s:
                async with s.get("https://api.ipify.org?format=text", timeout=10) as r:
                    return (await r.text()).strip()
        except Exception as e:
            LOG.error("get public IP failed: %s", e)
            return None


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(level=logging.INFO,
                        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s")
    ap = argparse.ArgumentParser(description="BCSP + LT-Seed cloud client demo")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p_pkg = sub.add_parser("package", help="演示 REST_Package 编解码")
    p_login = sub.add_parser("login", help="演示登录流程 (需真实服务器)")
    p_login.add_argument("--host", default="passport-client.bitcomet.com")
    p_login.add_argument("--port", type=int, default=25476)
    p_login.add_argument("--user", required=True)
    p_login.add_argument("--pass", dest="password", required=True)

    args = ap.parse_args()

    async def _pkg_demo():
        pkg = RESTPackage(name=RestName.LT_SEED_QUERY)
        pkg.payload = {"file_hash": "a" * 40}
        encoded = pkg.build()
        print(f"encoded {len(encoded)} bytes")
        decoded = RESTPackage.parse(encoded)
        if decoded:
            print(f"decoded: name={decoded.name.name} payload={decoded.payload}")
            print(f"is_response_ok={decoded.is_response_ok()}")

    async def _login_demo():
        if aiohttp is None:
            print("aiohttp required")
            return
        client = BCSPClient(server_host=args.host, server_port=args.port)
        await client.connect()
        ok = await client.login_password(args.user, args.password)
        if ok:
            print(f"✓ login success, user_id={client.user_id}")
            await client.logout()
        else:
            print("✗ login failed")
        await client.close()

    if args.cmd == "package":
        asyncio.run(_pkg_demo())
    elif args.cmd == "login":
        asyncio.run(_login_demo())
