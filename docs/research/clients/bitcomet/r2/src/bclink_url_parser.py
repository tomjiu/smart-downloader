"""
bclink_url_parser.py — 多协议 URL 统一解析器
================================================

逆向来源: BitComet `Core_Common::url_helper_bclink` 命名空间
对应符号 (demangled):
    url_helper_bclink::url_build(url_torrent_t, string_url, bool)
    url_helper_bclink::url_build(url_http_t,    string_url, bool)
    url_helper_bclink::url_build(url_ftp_t,     string_url, bool)
    url_helper_bclink::url_build(url_emule_t,   string_url, bool)
    url_helper_bclink::url_parse(string_url, url_protocol_enum)
    url_helper_bclink::url_decode(string_url::url_parts_t)

设计要点 (来自 nm -C 解析):
- BitComet 在一个 url_helper_bclink 命名空间里同时处理 4 种 URL 类型
- 用 url_protocol_enum 区分协议
- 一个 url_parts_t 结构统一表示解析结果
- qBittorrent 只支持 magnet/http/https，没有 url_emule_t / url_ftp_t

加速价值:
- 统一 URL 解析后，可以让同一个 download task 接受多源输入
- 用户从论坛复制 bc:// 链接，无需手动判断协议
- 后续 P2SP 多源下载器 (p2sp_downloader.py) 依赖此模块产出 SourceList

作者: Z.ai BitComet Reverse Engineering Team
"""
from __future__ import annotations

import base64
import binascii
import re
import urllib.parse as urlparse
from dataclasses import dataclass, field
from enum import Enum
from typing import List, Optional


class UrlProtocol(Enum):
    """对应 url_protocol_enum (逆向自 BitComet 符号表)."""
    HTTP = "http"
    HTTPS = "https"
    FTP = "ftp"
    MAGNET = "magnet"
    ED2K = "ed2k"
    BCLINK = "bc"          # BitComet 私有 bc:// 链接
    THUNDER = "thunder"    #迅雷链
    FLASHGET = "flashget"


@dataclass
class UrlParts:
    """对应 BitComet string_url::url_parts_t 结构.

    所有协议的解析结果都落到同一结构, 字段缺失时为 None.
    """
    protocol: UrlProtocol
    raw: str
    scheme: str = ""
    host: Optional[str] = None
    port: Optional[int] = None
    path: Optional[str] = None
    query: Optional[str] = None
    # BT / 磁链特有
    info_hash: Optional[str] = None    # 40-char hex
    name: Optional[str] = None
    size: Optional[int] = None
    # 多 tracker / 多源
    trackers: List[str] = field(default_factory=list)
    web_seeds: List[str] = field(default_factory=list)
    # ed2k 特有
    file_hash: Optional[str] = None     # ed2k hash (32 hex)
    # bc:// 私有
    bc_id: Optional[str] = None         # BitComet Content ID


# -----------------------------------------------------------------------------
# 解析器实现
# -----------------------------------------------------------------------------

_MAGNET_RE = re.compile(r"^magnet:\?(.+)$")
_ED2K_RE = re.compile(
    r"^ed2k://\|file\|([^|]+)\|(\d+)\|([0-9a-fA-F]{32})\|/?(?:\|s=([^|]*)\|/?)?$"
)
_THUNDER_RE = re.compile(r"^thunder://([A-Za-z0-9+/=]+)$")
_FLASHGET_RE = re.compile(r"^flashget://([A-Za-z0-9+/=]+)$")


def _decode_thunder(payload: str) -> str:
    """迅雷链 = base64('AA' + real_url + 'ZZ')."""
    try:
        raw = base64.b64decode(payload).decode("utf-8", errors="ignore")
        if raw.startswith("AA") and raw.endswith("ZZ"):
            return raw[2:-2]
    except (binascii.Error, ValueError):
        pass
    return ""


def parse(url: str) -> UrlParts:
    """统一入口: 任意 URL → UrlParts.

    对应 BitComet url_helper_bclink::url_parse(string_url, url_protocol_enum).
    """
    url = url.strip()
    low = url.lower()

    if low.startswith("magnet:"):
        return _parse_magnet(url)
    if low.startswith("ed2k://"):
        return _parse_ed2k(url)
    if low.startswith("thunder://"):
        return _parse_thunder(url)
    if low.startswith("flashget://"):
        return _parse_flashget(url)
    if low.startswith("bc://"):
        return _parse_bclink(url)
    if low.startswith(("http://", "https://")):
        return _parse_http(url)
    if low.startswith("ftp://"):
        return _parse_ftp(url)

    raise ValueError(f"unknown protocol: {url[:60]}")


def _parse_http(url: str) -> UrlParts:
    p = urlparse.urlsplit(url)
    return UrlParts(
        protocol=UrlProtocol.HTTPS if p.scheme == "https" else UrlProtocol.HTTP,
        raw=url, scheme=p.scheme, host=p.hostname,
        port=p.port, path=p.path or "/", query=p.query,
    )


def _parse_ftp(url: str) -> UrlParts:
    p = urlparse.urlsplit(url)
    return UrlParts(
        protocol=UrlProtocol.FTP, raw=url, scheme="ftp",
        host=p.hostname, port=p.port or 21, path=p.path or "/",
    )


def _parse_magnet(url: str) -> UrlParts:
    """对应 BitComet url_torrent_t (magnet variant)."""
    m = _MAGNET_RE.match(url)
    if not m:
        raise ValueError(f"invalid magnet: {url[:80]}")
    qs = urlparse.parse_qs(m.group(1))
    parts = UrlParts(protocol=UrlProtocol.MAGNET, raw=url, scheme="magnet")
    xt = qs.get("xt", [""])[0]
    if xt.lower().startswith("urn:btih:"):
        parts.info_hash = xt[8:].lower()
    parts.name = qs.get("dn", [None])[0]
    if "xl" in qs:
        try: parts.size = int(qs["xl"][0])
        except ValueError: pass
    parts.trackers = qs.get("tr", [])
    parts.web_seeds = qs.get("ws", [])
    return parts


def _parse_ed2k(url: str) -> UrlParts:
    """对应 BitComet url_emule_t."""
    m = _ED2K_RE.match(url)
    if not m:
        raise ValueError(f"invalid ed2k: {url[:80]}")
    name, size, fhash, sources = m.groups()
    parts = UrlParts(
        protocol=UrlProtocol.ED2K, raw=url, scheme="ed2k",
        name=name, size=int(size), file_hash=fhash.lower(),
    )
    if sources:
        parts.trackers = sources.split(",")
    return parts


def _parse_thunder(url: str) -> UrlParts:
    m = _THUNDER_RE.match(url)
    if not m:
        raise ValueError("invalid thunder link")
    inner = _decode_thunder(m.group(1))
    if not inner:
        raise ValueError("cannot decode thunder payload")
    # 迅雷内层通常是 http/ftp/ed2k, 递归解析
    inner_parts = parse(inner)
    inner_parts.raw = url  # 保留原始外壳
    return inner_parts


def _parse_flashget(url: str) -> UrlParts:
    m = _FLASHGET_RE.match(url)
    if not m:
        raise ValueError("invalid flashget link")
    # flashget 用类似迅雷的封装, base64 头尾标记为 "[FLASHGET]"
    try:
        raw = base64.b64decode(m.group(1)).decode("utf-8", errors="ignore")
        if raw.startswith("[FLASHGET]") and raw.endswith("[/FLASHGET]"):
            inner = raw[10:-11]
            return parse(inner)
    except (binascii.Error, ValueError):
        pass
    raise ValueError("cannot decode flashget payload")


def _parse_bclink(url: str) -> UrlParts:
    """bc:// BitComet 私有链接.

    格式: bc://<base64-payload-or-infohash>/<name>
    通常 bc:// 只是 magnet 的封装, 解码后回退到 magnet 解析.
    """
    p = urlparse.urlsplit(url)
    host = p.hostname or ""
    path = p.path.lstrip("/")
    parts = UrlParts(
        protocol=UrlProtocol.BCLINK, raw=url, scheme="bc",
        host=p.hostname, bc_id=host,
    )
    # 常见形式 1: bc://infohash/name
    if re.fullmatch(r"[0-9a-fA-F]{40}", host):
        parts.info_hash = host.lower()
        parts.name = urlparse.unquote(path) if path else None
        return parts
    # 常见形式 2: bc://base64(magnet) (BitComet 私有)
    try:
        decoded = base64.urlsafe_b64decode(host + "==").decode("utf-8", errors="ignore")
        if decoded.lower().startswith("magnet:"):
            inner = parse(decoded)
            # 保留外层 bc:// 信息
            inner.protocol = UrlProtocol.BCLINK
            inner.raw = url
            inner.bc_id = host
            return inner
    except (binascii.Error, ValueError):
        pass
    return parts


# -----------------------------------------------------------------------------
# 工具函数 (对应 url_helper_bclink 工具集)
# -----------------------------------------------------------------------------

def build(parts: UrlParts) -> str:
    """url_build 的反向操作."""
    if parts.protocol == UrlProtocol.MAGNET:
        return _build_magnet(parts)
    if parts.protocol in (UrlProtocol.HTTP, UrlProtocol.HTTPS):
        return _build_http(parts)
    if parts.protocol == UrlProtocol.FTP:
        return _build_ftp(parts)
    if parts.protocol == UrlProtocol.ED2K:
        return _build_ed2k(parts)
    if parts.protocol == UrlProtocol.BCLINK:
        return _build_bclink(parts)
    raise ValueError(f"cannot build protocol: {parts.protocol}")


def _build_http(p: UrlParts) -> str:
    netloc = p.host or ""
    if p.port and p.port not in (80, 443):
        netloc += f":{p.port}"
    return urlparse.urlunsplit((p.scheme, netloc, p.path or "/", p.query, ""))


def _build_ftp(p: UrlParts) -> str:
    netloc = p.host or ""
    if p.port and p.port != 21:
        netloc += f":{p.port}"
    return urlparse.urlunsplit(("ftp", netloc, p.path or "/", "", ""))


def _build_magnet(p: UrlParts) -> str:
    if not p.info_hash:
        raise ValueError("magnet requires info_hash")
    q = [("xt", f"urn:btih:{p.info_hash}")]
    if p.name:
        q.append(("dn", p.name))
    if p.size:
        q.append(("xl", str(p.size)))
    for tr in p.trackers:
        q.append(("tr", tr))
    for ws in p.web_seeds:
        q.append(("ws", ws))
    qs = "&".join(f"{k}={urlparse.quote(v, safe='')}" for k, v in q)
    return f"magnet:?{qs}"


def _build_ed2k(p: UrlParts) -> str:
    if not (p.name and p.size and p.file_hash):
        raise ValueError("ed2k requires name + size + file_hash")
    return f"ed2k://|file|{p.name}|{p.size}|{p.file_hash}|/|"


def _build_bclink(p: UrlParts) -> str:
    if p.info_hash:
        name = urlparse.quote(p.name or "")
        return f"bc://{p.info_hash}/{name}"
    raise ValueError("bc:// requires info_hash")


def is_valid(url: str) -> bool:
    """对应 url_helper_bclink::url_is_valid."""
    try:
        parse(url)
        return True
    except (ValueError, AttributeError):
        return False


# -----------------------------------------------------------------------------
# CLI 自测入口
# -----------------------------------------------------------------------------

if __name__ == "__main__":
    import sys
    if len(sys.argv) < 2:
        print("Usage: bclink_url_parser.py <url>")
        sys.exit(1)
    result = parse(sys.argv[1])
    for k, v in result.__dict__.items():
        if v:
            print(f"  {k:14s} : {v}")
