#!/usr/bin/env python3
"""cid_store.dat 结构扫描器（附录 A #7 · 2026-08-30）。

用途：拿到真实 cid_store.dat 样本后运行本脚本，产出结构化报告
（JSON）用于校准 crates/xunlei-convert/src/cid_store.rs 的假设解析器。

隐私（sample_collection_guide.md 口径）：本脚本**只读本地文件**，报告
默认脱敏 —— hash 只输出前 8 hex + 长度，路径只输出字符数与扩展名。
需要完整内容时显式传 --no-redact（仅限自有文件）。

用法：
    python3 cidstore_scan.py <path/to/cid_store.dat> [-o report.json] [--no-redact]
"""
import json
import math
import sys
from collections import Counter
from pathlib import Path


def entropy(data: bytes) -> float:
    if not data:
        return 0.0
    counts = Counter(data)
    n = len(data)
    return -sum((c / n) * math.log2(c / n) for c in counts.values())


def printable_ratio(data: bytes) -> float:
    if not data:
        return 0.0
    ok = sum(1 for b in data if 0x20 <= b < 0x7F or b in (9, 10, 13))
    return ok / len(data)


def ascii_strings(data: bytes, min_len: int = 6):
    out, start = [], None
    for i, b in enumerate(data):
        if 0x20 <= b < 0x7F:
            if start is None:
                start = i
        else:
            if start is not None and i - start >= min_len:
                out.append((start, data[start:i].decode("ascii", "ignore")))
            start = None
    if start is not None and len(data) - start >= min_len:
        out.append((start, data[start:].decode("ascii", "ignore")))
    return out


def utf16le_strings(data: bytes, min_chars: int = 6):
    out, start, buf = [], None, []
    i = 0
    while i + 1 < len(data):
        u = int.from_bytes(data[i : i + 2], "little")
        if 0x20 <= u < 0x7F or 0x4E00 <= u < 0xA000:
            if start is None:
                start = i
            buf.append(u)
            i += 2
        else:
            if start is not None and len(buf) >= min_chars:
                s = "".join(chr(c) for c in buf)
                if "/" in s or "\\" in s or "." in s:
                    out.append((start, s))
            start, buf = None, []
            i += 2
    if start is not None and len(buf) >= min_chars:
        s = "".join(chr(c) for c in buf)
        if "/" in s or "\\" in s or "." in s:
            out.append((start, s))
    return out


def redact(s: str) -> str:
    if len(s) <= 4:
        return "*" * len(s)
    ext = Path(s).suffix
    return f"{s[:2]}…<{len(s)}ch>{ext}" if ext else f"{s[:2]}…<{len(s)}ch>"


def hash_like_pairs(data: bytes, all_paths):
    """hash 候选（16/20/32B 非打印随机块）× 64B 窗口内最近路径。"""
    pairs = []
    paths = sorted(all_paths, key=lambda x: x[0])
    for hlen in (16, 20, 32):
        i = 0
        while i + hlen <= len(data):
            win = data[i : i + hlen]
            if not all(0x20 <= b < 0x7F for b in win):
                distinct = len(set(win))
                if distinct >= hlen * 0.6:
                    near = [p for p in paths if i < p[0] <= i + 64 + hlen]
                    near += [p for p in paths[::-1] if p[0] < i <= p[0] + 64]
                    if near:
                        pairs.append({"offset": i, "hash_len": hlen})
            i += 1
    return pairs


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(1)
    src = Path(sys.argv[1])
    out_path = None
    no_redact = "--no-redact" in sys.argv
    if "-o" in sys.argv:
        out_path = sys.argv[sys.argv.index("-o") + 1]
    data = src.read_bytes()

    report = {
        "file": str(src),
        "size": len(data),
        "magic8_hex": data[:8].hex(),
        "magic16_hex": data[:16].hex(),
        "entropy": round(entropy(data[: 1 << 20]), 3),
        "printable_ratio": round(printable_ratio(data), 4),
        "xdlctx_family": data.startswith(b"XDLCTX"),
        "json_like": b"{" in data[:64],
    }
    a_str = ascii_strings(data)
    u_str = utf16le_strings(data)
    paths = [(o, s) for o, s in a_str + u_str if "/" in s or "\\" in s or "." in s]
    report["ascii_string_count"] = len(a_str)
    report["utf16le_string_count"] = len(u_str)
    report["path_like_count"] = len(paths)
    if not no_redact:
        report["path_samples_redacted"] = [redact(s) for _, s in paths[:20]]
        report["ascii_head_redacted"] = [redact(s) for _, s in a_str[:20]]
    else:
        report["path_samples"] = [s for _, s in paths[:50]]
        report["ascii_head"] = [s for _, s in a_str[:50]]
    report["hash_like_pairs"] = hash_like_pairs(data, paths)[:200]

    report["interpretation_hints"] = [
        "json_like=true → 用 jq 人工看顶层键名，回填 cid_store.rs HASH_KEYS/PATH_KEYS",
        "xdlctx_family=true → 对照 xlbt_cfg.rs TLV 规格走查",
        "否则看 magic16_hex + entropy：>7.5 疑似压缩/加密（记录并止损）",
        "校准目标：cid_store.rs 的三形态判定与 {16,20,32} hash 长度集",
    ]

    text = json.dumps(report, ensure_ascii=False, indent=2)
    if out_path:
        Path(out_path).write_text(text, encoding="utf-8")
        print(f"report → {out_path}")
    print(text)


if __name__ == "__main__":
    main()
