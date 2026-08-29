"""
anti_leech_filter.py — BitComet AntiLeech 分级反吸血过滤器
=======================================================

逆向来源: BitComet `Core_BitTorrent::AntiLeechLevel` 枚举
关键符号:
    BitTorrentTaskWrapper::task_set_anti_leech_level(optional<AntiLeechLevel>)
    BitTorrentTask::get_anti_leech_level()
    配置项: anti_leech_level (从 strings 提取)

设计核心 (从符号分析):
1. 反吸血不是 0/1 开关, 而是分级 (AntiLeechLevel 枚举)
2. 客户端识别: peer_id / User-Agent / 行为模式
3. 限速分级: 低等级只是限速, 高等级直接 ban
4. 客户端过滤 + IP 过滤 + 协议过滤三层

加速价值 (针对 qBittorrent):
- qBittorrent 仅靠 libtorrent 内置 IP filter, 不识别客户端身份
- 公网 BT 任务经常被迅雷吸血客户端拉低速度
- AntiLeech 分级: 软限速 → 硬限速 → 拒绝连接

本模块提供:
- AntiLeechLevel 枚举 (4 级)
- 客户端指纹识别 (peer_id + User-Agent 模式匹配)
- 分级处理策略

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import logging
import re
import time
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Dict, List, Optional, Tuple

LOG = logging.getLogger("antileech")


# -----------------------------------------------------------------------------
# AntiLeechLevel — 对应 BitComet::AntiLeechLevel 枚举
# -----------------------------------------------------------------------------

class AntiLeechLevel(IntEnum):
    """反吸血等级 (从符号分析 + BitComet UI 行为反推).

    等级递进, 高等级包含低等级的所有措施.
    """
    OFF = 0                # 完全关闭, 不识别不限制
    SOFT = 1               # 识别 leech 客户端, 仅记录不限制
    LIMIT = 2              # 限速到 1/4 上传带宽
    AGGRESSIVE = 3         # 限速 + 拒绝新 piece 请求
    BAN = 4                # 完全 ban, 主动断开连接


# -----------------------------------------------------------------------------
# 客户端识别 — peer_id prefix (Azureus-style) + User-Agent
# -----------------------------------------------------------------------------

# BT 客户端 peer_id 前缀表 (Azureus-style: -XX####-)
# 来源: BT 协议规范 + BitComet 内部行为
KNOWN_CLIENTS = {
    # 主流正常客户端 (默认友好)
    "AZ": "Azureus / Vuze",
    "BT": "BitComet (官方, 友好)",
    "BC": "BitComet (官方, 友好)",
    "DE": "Deluge",
    "FD": "FoxTorrent",
    "FL": "Folx",
    "LT": "libtorrent (qBittorrent/rTorrent 等)",
    "qB": "qBittorrent",
    "RT": "rtorrent",
    "TR": "Transmission",
    "UT": "uTorrent",
    "UM": "uTorrent Mac",
    "UW": "uTorrent Web",
    "XL": "迅雷 (XunLei)",
    "XF": "Xfplay",
    "SD": "迅雷 (Thunder Mini)",
    "QQ": "QQDownload",
    "NX": "Net Transport",
    "TS": "Torrentstorm",
    "XX": "Xtorrent",
    "ZT": "ZipTorrent",
}

# 黑名单: 已知吸血/恶意客户端
LEECH_CLIENTS = {
    "XL": "迅雷 (XunLei) — 高优先下载, 不回报上传, 长期占用 peer slot",
    "SD": "迅雷 Mini — 同上, 行为更激进",
    "XF": "Xfplay — 流媒体下载, 通常不下完即离开",
    "QQ": "QQDownload — 腾讯下载, 不回报",
    "NX": "Net Transport — 多协议下载器, 上传吝啬",
    "TS": "Torrentstorm — 老旧客户端, 协议缺陷",
}

# User-Agent 黑名单 (HTTP webseed / LT-Seed 协议层)
LEECH_UA_PATTERNS = [
    re.compile(r"thunder", re.I),
    re.compile(r"xunlei", re.I),
    re.compile(r"qqdownload", re.I),
    re.compile(r"flashget", re.I),
    re.compile(r"xfplay", re.I),
    re.compile(r"net\s*transport", re.I),
    re.compile(r"ida(\s|$)", re.I),     # Internet Download Accelerator
    re.compile(r"Internet\s*Download\s*Manager", re.I),
    re.compile(r"\bIDM\b"),
]


# -----------------------------------------------------------------------------
# Peer 状态记录 — 用于行为分析
# -----------------------------------------------------------------------------

@dataclass
class PeerRecord:
    """单个 peer 的行为记录."""
    endpoint: Tuple[str, int]    # (ip, port)
    peer_id: str                  # 20-char BT peer_id
    user_agent: Optional[str] = None
    client_name: Optional[str] = None

    # 行为统计
    bytes_downloaded: int = 0     # 我们从他下载
    bytes_uploaded: int = 0       # 我们上传给他
    pieces_requested: int = 0     # 他请求的 piece 数
    pieces_served: int = 0        # 我们实际给他的 piece 数
    connect_time: float = field(default_factory=time.time)
    last_activity: float = field(default_factory=time.time)
    snubbed_count: int = 0        # 不响应我们 piece 请求的次数
    is_choking_us: bool = True
    is_interested: bool = False

    # 健康度评分
    score: float = 100.0

    @property
    def upload_download_ratio(self) -> float:
        if self.bytes_downloaded == 0:
            return float("inf") if self.bytes_uploaded > 0 else 0.0
        return self.bytes_uploaded / self.bytes_downloaded

    @property
    def seconds_connected(self) -> float:
        return time.time() - self.connect_time


def identify_client(peer_id, user_agent: Optional[str] = None) -> Tuple[str, str]:
    """识别 BT 客户端.

    Args:
        peer_id: 20-byte BT peer_id (bytes 或 str 都可以)
        user_agent: 可选 HTTP UA

    Returns:
        (client_code, client_name)
    """
    if isinstance(peer_id, bytes):
        peer_id_str = peer_id.decode("ascii", errors="replace")
    else:
        peer_id_str = peer_id
    # 兼容测试输入: 不足 20 字节的也允许
    # Azureus-style: -XX####-... (8 char prefix), 后面通常补齐到 20 字节
    m = re.match(r"^-([A-Za-z][A-Za-z])[0-9A-Za-z]{4}-", peer_id_str)
    if m:
        code = m.group(1).upper()
        name = KNOWN_CLIENTS.get(code, f"unknown ({code})")
        return code, name
    # Shadow style: 1 char prefix + 5 chars version + ----
    if len(peer_id_str) >= 6 and peer_id_str[0] in "ABCOSMRT":
        prefix_map = {"A": "ABC", "O": "Old_BITTORNADO", "S": "Shadow"}
        return peer_id_str[0], prefix_map.get(peer_id_str[0], "shadow-style")
    # Mainline style
    if peer_id_str.startswith("M1-") or peer_id_str.startswith("M2-"):
        return "M", "BitTorrent Mainline"
    return "??", "unknown"


def is_leech_client(client_code: str, user_agent: Optional[str] = None) -> bool:
    """判定客户端是否为吸血客户端."""
    if client_code in LEECH_CLIENTS:
        return True
    if user_agent:
        for pat in LEECH_UA_PATTERNS:
            if pat.search(user_agent):
                return True
    return False


# -----------------------------------------------------------------------------
# AntiLeech 处理器 — 分级策略
# -----------------------------------------------------------------------------

class AntiLeechFilter:
    """AntiLeech 过滤器 (主类).

    与 libtorrent 集成方式:
        在 libtorrent::torrent_handle 的 peer_alert 回调中调用本类.
        本类不会主动断开 peer, 而是返回一个 action, 由调用方执行.
    """

    def __init__(self, level: AntiLeechLevel = AntiLeechLevel.LIMIT,
                 max_score_threshold: float = 30.0,
                 min_share_ratio: float = 0.3,
                 snub_threshold: int = 3):
        self.level = level
        self.max_score_threshold = max_score_threshold
        self.min_share_ratio = min_share_ratio
        self.snub_threshold = snub_threshold
        # endpoint → PeerRecord
        self._peers: Dict[Tuple[str, int], PeerRecord] = {}

    # ----- 公开 API -----

    def add_peer(self, endpoint: Tuple[str, int], peer_id: bytes,
                 user_agent: Optional[str] = None) -> PeerRecord:
        code, name = identify_client(peer_id, user_agent)
        rec = PeerRecord(
            endpoint=endpoint, peer_id=peer_id.decode("ascii", errors="replace") if isinstance(peer_id, bytes) else peer_id,
            user_agent=user_agent, client_name=name,
        )
        # 标记客户端代码
        rec.client_name = f"{name} [{code}]"
        self._peers[endpoint] = rec
        return rec

    def update_stats(self, endpoint: Tuple[str, int],
                      downloaded: int = 0, uploaded: int = 0,
                      is_choking_us: Optional[bool] = None,
                      is_interested: Optional[bool] = None,
                      snubbed: bool = False) -> None:
        rec = self._peers.get(endpoint)
        if rec is None:
            return
        rec.bytes_downloaded += downloaded
        rec.bytes_uploaded += uploaded
        if is_choking_us is not None:
            rec.is_choking_us = is_choking_us
        if is_interested is not None:
            rec.is_interested = is_interested
        if snubbed:
            rec.snubbed_count += 1
        rec.last_activity = time.time()
        self._recalculate_score(rec)

    def decide(self, endpoint: Tuple[str, int]) -> "AntiLeechAction":
        """根据当前等级和 peer 行为, 返回应采取的动作."""
        if self.level == AntiLeechLevel.OFF:
            return AntiLeechAction.ALLOW

        rec = self._peers.get(endpoint)
        if rec is None:
            return AntiLeechAction.ALLOW

        code, _ = identify_client(rec.peer_id.encode(), rec.user_agent)
        is_leech = is_leech_client(code, rec.user_agent)

        # 等级 1 (SOFT): 仅记录
        if self.level == AntiLeechLevel.SOFT:
            if is_leech:
                LOG.info("leech detected (soft): %s %s", endpoint, rec.client_name)
            return AntiLeechAction.ALLOW

        # 等级 2 (LIMIT): 限速到 1/4
        if self.level == AntiLeechLevel.LIMIT:
            if is_leech or rec.score < self.max_score_threshold:
                return AntiLeechAction.LIMIT_25
            return AntiLeechAction.ALLOW

        # 等级 3 (AGGRESSIVE): 限速 + 拒绝新 piece
        if self.level == AntiLeechLevel.AGGRESSIVE:
            if is_leech:
                return AntiLeechAction.BAN_NEW_REQUESTS
            if rec.score < self.max_score_threshold or rec.snubbed_count > self.snub_threshold:
                return AntiLeechAction.LIMIT_25
            return AntiLeechAction.ALLOW

        # 等级 4 (BAN): 直接 ban
        if self.level == AntiLeechLevel.BAN:
            if is_leech or rec.score < self.max_score_threshold:
                return AntiLeechAction.DISCONNECT
            return AntiLeechAction.ALLOW

        return AntiLeechAction.ALLOW

    def get_stats(self) -> Dict[str, int]:
        stats = {"total": 0, "leech": 0, "good": 0, "banned": 0}
        for rec in self._peers.values():
            stats["total"] += 1
            code, _ = identify_client(rec.peer_id.encode(), rec.user_agent)
            if is_leech_client(code, rec.user_agent):
                stats["leech"] += 1
            elif rec.score > 70:
                stats["good"] += 1
        return stats

    # ----- 内部 -----

    def _recalculate_score(self, rec: PeerRecord) -> None:
        """计算 peer 健康度评分 (0-100)."""
        score = 100.0
        # 1. 上传/下载比 (核心指标)
        if rec.bytes_downloaded > 0:
            ratio = rec.upload_download_ratio
            if ratio < self.min_share_ratio:
                score -= (self.min_share_ratio - ratio) * 100
        # 2. snub 次数
        if rec.snubbed_count > self.snub_threshold:
            score -= (rec.snubbed_count - self.snub_threshold) * 5
        # 3. 长期 choked 我们
        if rec.is_choking_us and rec.seconds_connected > 60:
            score -= 20
        # 4. 客户端身份
        code, _ = identify_client(rec.peer_id.encode(), rec.user_agent)
        if is_leech_client(code, rec.user_agent):
            score -= 40
        rec.score = max(0.0, min(100.0, score))


class AntiLeechAction(IntEnum):
    """AntiLeech 决策结果."""
    ALLOW = 0                # 正常对待
    LIMIT_25 = 1             # 限速到 25% 上传带宽
    BAN_NEW_REQUESTS = 2     # 拒绝新的 piece 请求 (但保留已发请求)
    DISCONNECT = 3           # 直接断开


# -----------------------------------------------------------------------------
# 与 libtorrent 集成的 hook 函数 (可选)
# -----------------------------------------------------------------------------

def libtorrent_peer_hook(filter: AntiLeechFilter, torrent_handle, peer_info) -> AntiLeechAction:
    """libtorrent peer_alert 回调 hook.

    使用方式:
        def on_peer_alert(alert):
            handle = alert.handle
            peer_info = handle.peer_info(alert.endpoints[0])
            action = libtorrent_peer_hook(my_filter, handle, peer_info)
            if action == AntiLeechAction.DISCONNECT:
                handle.disconnect_peer(peer_info)
            elif action == AntiLeechAction.LIMIT_25:
                handle.set_peer_upload_limit(peer_info, default_limit // 4)
    """
    try:
        import libtorrent as lt
    except ImportError:
        return AntiLeechAction.ALLOW

    endpoint = (peer_info.ip[0], peer_info.ip[1])
    peer_id = peer_info.pid.to_bytes() if hasattr(peer_info.pid, 'to_bytes') else bytes(peer_info.pid)
    # 找 UA (libtorrent 不暴露, 留空)
    if endpoint not in filter._peers:
        filter.add_peer(endpoint, peer_id)
    filter.update_stats(
        endpoint,
        downloaded=peer_info.total_download,
        uploaded=peer_info.total_upload,
        is_choking_us=peer_info.flags & peer_info.choked,
        is_interested=peer_info.flags & peer_info.interested,
        snubbed=peer_info.flags & peer_info.snubbed,
    )
    return filter.decide(endpoint)


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )
    ap = argparse.ArgumentParser(description="AntiLeech filter demo")
    ap.add_argument("--level", type=int, default=2,
                    help="0=off 1=soft 2=limit 3=aggressive 4=ban")
    args = ap.parse_args()

    f = AntiLeechFilter(level=AntiLeechLevel(args.level))

    # 模拟一个迅雷 peer
    rec = f.add_peer(("1.2.3.4", 6881), b"-XL0001-abcdefg")
    print(f"identified: {rec.client_name}")
    f.update_stats(("1.2.3.4", 6881), downloaded=1000000, uploaded=1000)
    action = f.decide(("1.2.3.4", 6881))
    print(f"action for XunLei peer at level {args.level}: {action.name}")

    # 模拟一个 qBittorrent peer
    rec2 = f.add_peer(("5.6.7.8", 6881), b"-qB4500-abcdefgh")
    f.update_stats(("5.6.7.8", 6881), downloaded=500000, uploaded=800000)
    action2 = f.decide(("5.6.7.8", 6881))
    print(f"action for qBittorrent peer: {action2.name}")

    print("stats:", f.get_stats())
