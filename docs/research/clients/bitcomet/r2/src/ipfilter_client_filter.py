"""
ipfilter_client_filter.py — BitComet IP filter + 客户端过滤器
========================================================

逆向来源: Core_BitTorrent::BitTorrentSettings + BitTorrentSettingsCallback
关键符号:
    BitTorrentSettings::client_filter_clear_rule
    BitTorrentSettings::client_filter_export_file_content
    BitTorrentSettings::client_filter_get_rules
    BitTorrentSettings::client_filter_get_stats
    BitTorrentSettings::client_filter_httpclient_visit_finished
    BitTorrentSettings::client_filter_import_file_content
    BitTorrentSettings::client_filter_load_from_data_file
    BitTorrentSettings::client_filter_reload_from_user_file
    BitTorrentSettings::client_filter_set_data_file_path
    BitTorrentSettings::client_filter_set_rules
    BitTorrentSettings::client_filter_update
    BitTorrentSettings::client_filter_update_auto
    BitTorrentSettings::download_client_filter
    BitTorrentSettings::download_ipfilter
    BitTorrentSettings::get_client_filter_action
    BitTorrentSettings::get_settings_client_filter
    BitTorrentSettings::get_settings_ipfilter
    BitTorrentSettings::ipfilter_append_to_manual_list
    BitTorrentSettings::ipfilter_clear
    BitTorrentSettings::ipfilter_export_file_content
    BitTorrentSettings::ipfilter_get_manual_list
    BitTorrentSettings::ipfilter_get_stats
    BitTorrentSettings::ipfilter_httpclient_visit_finished
    BitTorrentSettings::ipfilter_import_file_content
    BitTorrentSettings::ipfilter_load_from_data_file
    BitTorrentSettings::ipfilter_reload_from_user_file
    BitTorrentSettings::ipfilter_set_data_file_path
    BitTorrentSettings::ipfilter_set_manual_list
    BitTorrentSettings::ipfilter_update
    BitTorrentSettings::ipfilter_update_auto
    BitTorrentSettings::is_peer_ip_refused
    BitTorrentSettings::is_peerid_refused
    BitTorrentSettings::on_client_filter_received
    BitTorrentSettings::on_ipfilter_received
    BitTorrentSettings::set_client_filter_last_update
    BitTorrentSettings::set_ipfilter_last_update_time
    BitTorrentSettings::set_refused_client_types
    BitTorrentSettings::set_settings_client_filter
    BitTorrentSettings::set_settings_ipfilter

    BitTorrentSettingsCallback::on_client_filter_list_loaded
    BitTorrentSettingsCallback::on_client_filter_list_updated
    BitTorrentSettingsCallback::on_ipfilter_list_loaded
    BitTorrentSettingsCallback::on_ipfilter_list_updated

    BitTorrent::client_filter_rule_list_t
    BitTorrent::settings_client_filter_t
    BitTorrent::settings_ipfilter_t
    BitTorrent::stats_client_filter_t
    BitTorrent::stats_ipfilter_t
    BitTorrent::PeerBannedReason

    Core_BitTorrent::_GLOBAL__N_::IncomingIPFilter

数据结构 (反推):
    struct client_filter_rule_list_t {
        vector<client_filter_rule_t> rules;
        string version;
        time_t last_update;
    };

    struct client_filter_rule_t {
        string client_code;        # e.g. "XL", "SD", "QQ"
        string peer_id_pattern;    # 正则或前缀
        string user_agent_pattern;
        action_enum action;         # ALLOW / LIMIT / BAN
        int rate_limit;            # 限速 (KB/s)
    };

    struct settings_client_filter_t {
        bool enable;
        bool auto_update;
        string data_file_path;
        string user_file_path;
    };

    struct settings_ipfilter_t {
        bool enable;
        bool auto_update;
        string data_file_path;
        string user_file_path;
    };

API 端点 (来自 strings):
    /api/config/client_filter/{clear,download,get,query,set,update,upload}
    /api/config/ipfilter/{clear,download,get,query,set,update,upload}

设计核心:
1. 双层过滤: IP filter + 客户端 filter
2. 自动更新 (auto_update): 从 BitComet 服务器下载最新规则
3. 用户自定义规则 (user_file_path)
4. 数据文件 (data_file_path): 缓存服务器规则
5. httpclient_visit_finished: 用 HTTP 客户端下载规则完成回调
6. on_*_list_loaded/updated: 加载/更新后通知 UI
7. PeerBannedReason: 详细 ban 原因

加速价值 (针对 qBittorrent):
- qBittorrent 仅支持 IP filter (libtorrent 内置), 不识别客户端
- BitComet 双层过滤让用户可:
  a) 按 peer_id 模式过滤 (e.g. 所有 -XL####- 客户端)
  b) 按 User-Agent 过滤 (迅雷/IDM 等)
  c) 按动作分级 (ALLOW / LIMIT / BAN)
- 自动更新让规则保持最新

本模块实现:
- IpFilterRule / ClientFilterRule: 规则数据结构
- IpFilter / ClientFilter: 过滤器主体
- FilterSettings + AutoUpdater: 设置 + 自动更新
- PeerBanRecord: ban 原因记录

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import hashlib
import ipaddress
import logging
import os
import re
import threading
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Callable, Deque, Dict, List, Optional, Set, Tuple

LOG = logging.getLogger("filter")


# -----------------------------------------------------------------------------
# 枚举
# -----------------------------------------------------------------------------

class FilterAction(IntEnum):
    """对应 client_filter_rule.action_enum."""
    ALLOW = 0
    LIMIT_25 = 1         # 限速到 25% 上传带宽
    LIMIT_50 = 2
    LIMIT_CUSTOM = 3     # 自定义限速
    BAN = 4               # 完全 ban


class PeerBannedReason(IntEnum):
    """对应 BitTorrent::PeerBannedReason."""
    NONE = 0
    IP_FILTERED = 1          # IP filter 命中
    CLIENT_FILTERED = 2      # 客户端 filter 命中
    SNUBBED_TOO_MANY = 3      # 长期不响应
    RATE_LIMIT_EXCEEDED = 4   # 超出限速
    ANTI_LEECH = 5            # AntiLeech 模块触发
    TOO_MANY_FAILURES = 6     # 失败次数过多
    USER_MANUAL = 7           # 用户手动 ban
    AUTOMATIC = 8              # 自动 ban (来自服务器规则)


# -----------------------------------------------------------------------------
# 数据结构
# -----------------------------------------------------------------------------

@dataclass
class IpFilterRule:
    """单条 IP 过滤规则."""
    ip_range: str           # CIDR 或 IP range (e.g. "192.168.1.0/24" or "10.0.0.1-10.0.0.99")
    action: FilterAction = FilterAction.BAN
    description: str = ""
    source: str = "manual"   # manual / auto / data_file
    added_at: float = field(default_factory=time.time)

    def matches(self, ip: str) -> bool:
        try:
            net = ipaddress.ip_network(self.ip_range, strict=False)
            addr = ipaddress.ip_address(ip)
            return addr in net
        except ValueError:
            # IP range 格式
            if "-" in self.ip_range:
                start, end = self.ip_range.split("-")
                try:
                    return ipaddress.ip_address(start) <= ipaddress.ip_address(ip) <= ipaddress.ip_address(end)
                except ValueError:
                    return False
            return False


@dataclass
class ClientFilterRule:
    """对应 client_filter_rule_t."""
    client_code: str = ""                  # e.g. "XL", "SD", "QQ"
    peer_id_pattern: str = ""             # 正则
    user_agent_pattern: str = ""           # 正则
    action: FilterAction = FilterAction.BAN
    rate_limit_kbps: int = 0              # LIMIT_CUSTOM 时用
    description: str = ""
    source: str = "manual"
    # 预编译正则
    _peer_id_re: Optional[re.Pattern] = None
    _user_agent_re: Optional[re.Pattern] = None

    def __post_init__(self):
        if self.peer_id_pattern:
            try:
                self._peer_id_re = re.compile(self.peer_id_pattern)
            except re.error as e:
                LOG.warning(f"invalid peer_id pattern '{self.peer_id_pattern}': {e}")
        if self.user_agent_pattern:
            try:
                self._user_agent_re = re.compile(self.user_agent_pattern)
            except re.error as e:
                LOG.warning(f"invalid user_agent pattern '{self.user_agent_pattern}': {e}")

    def matches(self, peer_id: bytes, user_agent: Optional[str] = None) -> bool:
        # 客户端代码匹配
        if self.client_code:
            if isinstance(peer_id, bytes):
                peer_id_str = peer_id.decode("ascii", errors="replace")
            else:
                peer_id_str = peer_id
            # Azureus-style: -XX####-
            if not peer_id_str.startswith(f"-{self.client_code}") and \
               len(peer_id_str) >= 8 and \
               not (peer_id_str[1:3] == self.client_code):
                return False
        # 正则匹配
        if self._peer_id_re:
            peer_id_str = peer_id.decode("ascii", errors="replace") if isinstance(peer_id, bytes) else peer_id
            if not self._peer_id_re.search(peer_id_str):
                return False
        if self._user_agent_re:
            if not user_agent:
                return False
            if not self._user_agent_re.search(user_agent):
                return False
        return True


@dataclass
class PeerBanRecord:
    """对应 PeerBannedReason + ban 记录."""
    endpoint: Tuple[str, int]
    reason: PeerBannedReason
    reason_str: str = ""
    banned_at: float = field(default_factory=time.time)
    expires_at: float = 0.0          # 0 = 永久
    rule_source: str = ""           # 哪个规则触发的


# -----------------------------------------------------------------------------
# IpFilter — IP 过滤器主体
# -----------------------------------------------------------------------------

class IpFilter:
    """对应 BitTorrentSettings 的 ipfilter_* 系列."""

    def __init__(self):
        # 规则列表
        self._rules: List[IpFilterRule] = []
        # 手动 ban 列表 (单独存储)
        self._manual_list: List[str] = []
        # 数据文件路径
        self._data_file: Optional[str] = None
        self._user_file: Optional[str] = None
        # 自动更新
        self._auto_update: bool = False
        self._last_update_time: float = 0.0
        # 回调
        self.on_list_loaded: Optional[Callable] = None
        self.on_list_updated: Optional[Callable] = None
        # 统计
        self._stats = {
            "checks": 0,
            "blocks": 0,
            "allows": 0,
        }

    # ----- 规则管理 -----

    def set_rules(self, rules: List[IpFilterRule]) -> None:
        """对应 ipfilter_set_manual_list."""
        self._rules = list(rules)
        if self.on_list_updated:
            self.on_list_updated()

    def add_rule(self, rule: IpFilterRule) -> None:
        self._rules.append(rule)

    def append_to_manual_list(self, ip_range: str,
                                description: str = "") -> None:
        """对应 ipfilter_append_to_manual_list."""
        self._manual_list.append(ip_range)
        self._rules.append(IpFilterRule(
            ip_range=ip_range,
            description=description,
            source="manual",
        ))

    def clear(self) -> None:
        """对应 ipfilter_clear."""
        self._rules.clear()
        self._manual_list.clear()
        if self.on_list_updated:
            self.on_list_updated()

    def get_rules(self) -> List[IpFilterRule]:
        return list(self._rules)

    def get_manual_list(self) -> List[str]:
        """对应 ipfilter_get_manual_list."""
        return list(self._manual_list)

    # ----- 文件持久化 -----

    def set_data_file_path(self, path: str) -> None:
        """对应 ipfilter_set_data_file_path."""
        self._data_file = path

    def reload_from_user_file(self, path: Optional[str] = None) -> int:
        """对应 ipfilter_reload_from_user_file."""
        path = path or self._user_file
        if not path or not os.path.exists(path):
            return 0
        count = 0
        with open(path, "r", encoding="utf-8", errors="ignore") as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("#"):
                    continue
                parts = line.split(":", 1)
                ip_range = parts[0].strip()
                desc = parts[1].strip() if len(parts) > 1 else ""
                self._rules.append(IpFilterRule(
                    ip_range=ip_range, description=desc, source="user_file"
                ))
                count += 1
        if self.on_list_loaded:
            self.on_list_loaded()
        return count

    def load_from_data_file(self) -> int:
        """对应 ipfilter_load_from_data_file."""
        if not self._data_file or not os.path.exists(self._data_file):
            return 0
        return self.reload_from_user_file(self._data_file)

    def import_file_content(self, content: str) -> int:
        """对应 ipfilter_import_file_content."""
        count = 0
        for line in content.splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split(":", 1)
            ip_range = parts[0].strip()
            desc = parts[1].strip() if len(parts) > 1 else ""
            self._rules.append(IpFilterRule(
                ip_range=ip_range, description=desc, source="imported"
            ))
            count += 1
        return count

    def export_file_content(self) -> str:
        """对应 ipfilter_export_file_content."""
        lines = []
        for r in self._rules:
            if r.description:
                lines.append(f"{r.ip_range}:{r.description}")
            else:
                lines.append(r.ip_range)
        return "\n".join(lines)

    # ----- 检查 -----

    def is_peer_ip_refused(self, ip: str) -> Tuple[bool, Optional[PeerBanRecord]]:
        """对应 is_peer_ip_refused."""
        self._stats["checks"] += 1
        for rule in self._rules:
            if rule.matches(ip):
                if rule.action == FilterAction.BAN:
                    self._stats["blocks"] += 1
                    return True, PeerBanRecord(
                        endpoint=(ip, 0),
                        reason=PeerBannedReason.IP_FILTERED,
                        reason_str=f"IP rule: {rule.ip_range}",
                        rule_source=rule.source,
                    )
                # ALLOW/LIMIT 不在这里处理, 由上层 client_filter 决定
        self._stats["allows"] += 1
        return False, None

    # ----- 自动更新 -----

    def set_auto_update(self, enabled: bool) -> None:
        self._auto_update = enabled

    def update_auto(self) -> None:
        """对应 ipfilter_update_auto."""
        if not self._auto_update:
            return
        # 简化: 不实际下载, 标记时间
        self._last_update_time = time.time()
        if self.on_list_updated:
            self.on_list_updated()

    def update(self, new_rules: List[IpFilterRule]) -> None:
        """对应 ipfilter_update."""
        self._rules = list(new_rules)
        self._last_update_time = time.time()
        if self.on_list_updated:
            self.on_list_updated()

    def httpclient_visit_finished(self, success: bool) -> None:
        """对应 ipfilter_httpclient_visit_finished."""
        if success:
            self._last_update_time = time.time()

    def get_stats(self) -> Dict:
        """对应 ipfilter_get_stats."""
        return {
            "rule_count": len(self._rules),
            "manual_count": len(self._manual_list),
            "last_update": self._last_update_time,
            "auto_update": self._auto_update,
            **self._stats,
        }


# -----------------------------------------------------------------------------
# ClientFilter — 客户端过滤器主体
# -----------------------------------------------------------------------------

class ClientFilter:
    """对应 BitTorrentSettings 的 client_filter_* 系列."""

    def __init__(self):
        # 规则列表
        self._rules: List[ClientFilterRule] = []
        # 拒绝的客户端类型 (set_refused_client_types)
        self._refused_types: Set[str] = set()
        # 数据文件
        self._data_file: Optional[str] = None
        self._user_file: Optional[str] = None
        # 自动更新
        self._auto_update: bool = False
        self._last_update: float = 0.0
        # 回调
        self.on_list_loaded: Optional[Callable] = None
        self.on_list_updated: Optional[Callable] = None
        # 统计
        self._stats = {
            "checks": 0,
            "matches": 0,
            "bans": 0,
            "limits": 0,
        }

    # ----- 规则管理 -----

    def set_rules(self, rules: List[ClientFilterRule]) -> None:
        """对应 client_filter_set_rules."""
        self._rules = list(rules)
        if self.on_list_updated:
            self.on_list_updated()

    def add_rule(self, rule: ClientFilterRule) -> None:
        self._rules.append(rule)

    def clear_rule(self) -> None:
        """对应 client_filter_clear_rule."""
        self._rules.clear()
        if self.on_list_updated:
            self.on_list_updated()

    def get_rules(self) -> List[ClientFilterRule]:
        """对应 client_filter_get_rules."""
        return list(self._rules)

    def set_refused_client_types(self, types: List[str]) -> None:
        """对应 set_refused_client_types."""
        self._refused_types = set(t.upper() for t in types)

    # ----- 文件 -----

    def set_data_file_path(self, path: str) -> None:
        self._data_file = path

    def reload_from_user_file(self, path: Optional[str] = None) -> int:
        path = path or self._user_file
        if not path or not os.path.exists(path):
            return 0
        count = 0
        with open(path, "r", encoding="utf-8", errors="ignore") as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith("#"):
                    continue
                # 格式: client_code:peer_id_pattern:user_agent_pattern:action:rate_limit:description
                parts = line.split(":", 5)
                if len(parts) < 4:
                    continue
                code, pid_pat, ua_pat, action_str = parts[:4]
                rate_limit = int(parts[4]) if len(parts) > 4 else 0
                desc = parts[5] if len(parts) > 5 else ""
                self._rules.append(ClientFilterRule(
                    client_code=code,
                    peer_id_pattern=pid_pat,
                    user_agent_pattern=ua_pat,
                    action=FilterAction(int(action_str)),
                    rate_limit_kbps=rate_limit,
                    description=desc,
                    source="user_file",
                ))
                count += 1
        if self.on_list_loaded:
            self.on_list_loaded()
        return count

    def load_from_data_file(self) -> int:
        if not self._data_file or not os.path.exists(self._data_file):
            return 0
        return self.reload_from_user_file(self._data_file)

    def import_file_content(self, content: str) -> int:
        count = 0
        for line in content.splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split(":", 5)
            if len(parts) < 4:
                continue
            code, pid_pat, ua_pat, action_str = parts[:4]
            rate_limit = int(parts[4]) if len(parts) > 4 else 0
            desc = parts[5] if len(parts) > 5 else ""
            self._rules.append(ClientFilterRule(
                client_code=code,
                peer_id_pattern=pid_pat,
                user_agent_pattern=ua_pat,
                action=FilterAction(int(action_str)),
                rate_limit_kbps=rate_limit,
                description=desc,
                source="imported",
            ))
            count += 1
        return count

    def export_file_content(self) -> str:
        lines = []
        for r in self._rules:
            lines.append(f"{r.client_code}:{r.peer_id_pattern}:{r.user_agent_pattern}:"
                        f"{int(r.action)}:{r.rate_limit_kbps}:{r.description}")
        return "\n".join(lines)

    # ----- 检查 -----

    def is_peerid_refused(self, peer_id: bytes,
                          user_agent: Optional[str] = None
                          ) -> Tuple[FilterAction, Optional[PeerBanRecord]]:
        """对应 is_peerid_refused."""
        self._stats["checks"] += 1
        # 1. 拒绝类型快速检查
        if isinstance(peer_id, bytes):
            peer_id_str = peer_id.decode("ascii", errors="replace")
        else:
            peer_id_str = peer_id
        if len(peer_id_str) >= 3:
            code = peer_id_str[1:3].upper() if peer_id_str[0] == "-" else ""
            if code in self._refused_types:
                self._stats["bans"] += 1
                return FilterAction.BAN, PeerBanRecord(
                    endpoint=("", 0),
                    reason=PeerBannedReason.CLIENT_FILTERED,
                    reason_str=f"client type {code} refused",
                    rule_source="refused_types",
                )
        # 2. 规则匹配
        for rule in self._rules:
            if rule.matches(peer_id, user_agent):
                self._stats["matches"] += 1
                if rule.action == FilterAction.BAN:
                    self._stats["bans"] += 1
                elif rule.action in (FilterAction.LIMIT_25, FilterAction.LIMIT_50,
                                       FilterAction.LIMIT_CUSTOM):
                    self._stats["limits"] += 1
                return rule.action, PeerBanRecord(
                    endpoint=("", 0),
                    reason=PeerBannedReason.CLIENT_FILTERED,
                    reason_str=f"rule: {rule.client_code} {rule.peer_id_pattern}",
                    rule_source=rule.source,
                )
        return FilterAction.ALLOW, None

    def get_action(self, peer_id: bytes,
                    user_agent: Optional[str] = None) -> FilterAction:
        """对应 get_client_filter_action."""
        action, _ = self.is_peerid_refused(peer_id, user_agent)
        return action

    # ----- 自动更新 -----

    def set_auto_update(self, enabled: bool) -> None:
        self._auto_update = enabled

    def update_auto(self) -> None:
        if not self._auto_update:
            return
        self._last_update = time.time()
        if self.on_list_updated:
            self.on_list_updated()

    def update(self, rules: List[ClientFilterRule]) -> None:
        self._rules = list(rules)
        self._last_update = time.time()
        if self.on_list_updated:
            self.on_list_updated()

    def httpclient_visit_finished(self, success: bool) -> None:
        if success:
            self._last_update = time.time()

    def set_last_update(self, ts: float) -> None:
        self._last_update = ts

    def get_stats(self) -> Dict:
        return {
            "rule_count": len(self._rules),
            "refused_types_count": len(self._refused_types),
            "last_update": self._last_update,
            "auto_update": self._auto_update,
            **self._stats,
        }


# -----------------------------------------------------------------------------
# CombinedFilter — 双层过滤器协调
# -----------------------------------------------------------------------------

class CombinedFilter:
    """对应 BitTorrentSettings 同时管理 IP filter + 客户端 filter."""

    def __init__(self):
        self.ip_filter = IpFilter()
        self.client_filter = ClientFilter()
        # ban 记录 (LRU, 最近 1000 条)
        self._ban_records: Deque[PeerBanRecord] = deque(maxlen=1000)
        # 临时 ban (来自 AntiLeech 等动态触发)
        self._temp_bans: Dict[Tuple[str, int], PeerBanRecord] = {}

    def check_peer(self, ip: str, port: int,
                    peer_id: bytes,
                    user_agent: Optional[str] = None
                    ) -> Tuple[FilterAction, Optional[PeerBanRecord]]:
        """综合检查 peer 是否应被过滤."""
        # 1. 临时 ban 检查
        ep = (ip, port)
        if ep in self._temp_bans:
            ban = self._temp_bans[ep]
            if ban.expires_at == 0 or time.time() < ban.expires_at:
                return FilterAction.BAN, ban
            else:
                del self._temp_bans[ep]
        # 2. IP filter
        ip_banned, ip_record = self.ip_filter.is_peer_ip_refused(ip)
        if ip_banned:
            self._ban_records.append(ip_record)
            return FilterAction.BAN, ip_record
        # 3. 客户端 filter
        action, client_record = self.client_filter.is_peerid_refused(peer_id, user_agent)
        if action != FilterAction.ALLOW:
            if client_record:
                client_record.endpoint = ep
                self._ban_records.append(client_record)
            return action, client_record
        return FilterAction.ALLOW, None

    def add_temp_ban(self, endpoint: Tuple[str, int], reason: PeerBannedReason,
                      duration_sec: float = 3600,
                      reason_str: str = "") -> None:
        """添加临时 ban (来自 AntiLeech 等动态触发)."""
        self._temp_bans[endpoint] = PeerBanRecord(
            endpoint=endpoint,
            reason=reason,
            reason_str=reason_str,
            expires_at=time.time() + duration_sec if duration_sec > 0 else 0,
            rule_source="temp",
        )

    def remove_temp_ban(self, endpoint: Tuple[str, int]) -> None:
        self._temp_bans.pop(endpoint, None)

    def get_ban_records(self, limit: int = 100) -> List[PeerBanRecord]:
        """获取最近 ban 记录."""
        return list(self._ban_records)[-limit:]

    def get_stats(self) -> Dict:
        return {
            "ip_filter": self.ip_filter.get_stats(),
            "client_filter": self.client_filter.get_stats(),
            "temp_bans": len(self._temp_bans),
            "ban_records": len(self._ban_records),
        }


# -----------------------------------------------------------------------------
# CLI
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import argparse
    print("=" * 60)
    print("BitComet IP filter + Client filter demo")
    print("=" * 60)
    combined = CombinedFilter()
    # 添加 IP filter 规则
    combined.ip_filter.add_rule(IpFilterRule(
        ip_range="10.0.0.0/8", description="private net"
    ))
    combined.ip_filter.add_rule(IpFilterRule(
        ip_range="192.168.0.0/16", description="home net"
    ))
    combined.ip_filter.add_rule(IpFilterRule(
        ip_range="1.2.3.4", description="specific IP"
    ))
    # 添加 client filter 规则
    combined.client_filter.add_rule(ClientFilterRule(
        client_code="XL", peer_id_pattern=r"-XL\d{4}-",
        action=FilterAction.LIMIT_25, description="XunLei limit"
    ))
    combined.client_filter.add_rule(ClientFilterRule(
        client_code="SD", peer_id_pattern=r"-SD\d{4}-",
        action=FilterAction.BAN, description="XunLei Mini ban"
    ))
    combined.client_filter.set_refused_client_types(["QQ", "XF"])
    # 测试
    test_cases = [
        ("1.2.3.4", 6881, b"-XL0001-abcdefghij", None),       # IP ban
        ("8.8.8.8", 6881, b"-XL0001-abcdefghij", None),          # client limit
        ("8.8.8.8", 6881, b"-SD0001-abcdefghij", None),          # client ban
        ("8.8.8.8", 6881, b"-qB4500-abcdefghij", None),          # allow
        ("8.8.8.8", 6881, b"-QQ0001-abcdefghij", None),          # refused type
        ("192.168.1.1", 6881, b"-qB4500-abcdefghij", None),     # IP ban (private)
    ]
    for ip, port, pid, ua in test_cases:
        action, record = combined.check_peer(ip, port, pid, ua)
        pid_str = pid[:8].decode("ascii", errors="replace")
        reason = record.reason_str if record else ""
        print(f"  {ip:15s}:{port} pid={pid_str} → action={action.name:8s} reason={reason}")
    # 临时 ban
    combined.add_temp_ban(("9.9.9.9", 6881), PeerBannedReason.ANTI_LEECH,
                            duration_sec=3600, reason_str="AntiLeech BAN level")
    action, _ = combined.check_peer("9.9.9.9", 6881, b"-qB4500-abcdefghij")
    print(f"\n  临时 ban 测试: 9.9.9.9 → {action.name}")
    # 统计
    print(f"\n=== Stats ===")
    for k, v in combined.get_stats().items():
        if isinstance(v, dict):
            print(f"  {k}:")
            for k2, v2 in v.items():
                print(f"    {k2}: {v2}")
        else:
            print(f"  {k}: {v}")
