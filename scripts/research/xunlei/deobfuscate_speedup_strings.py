#!/usr/bin/env python3
"""
反混淆迅雷前端 JS dump 中的字符串拼接，提取 speedup / VipSpeedUpUrl / HostHighSpeedFlow
相关路径。

用法:
    python deobfuscate_speedup_strings.py
    python deobfuscate_speedup_strings.py --output speedup_strings.md

输出:
    - 终端: 所有命中 speed 相关关键字的拼接字符串还原结果
    - 文件: Markdown 报告（含文件:行号 | 还原字符串 | 原始片段）
"""

import re
import os
import sys
import argparse
from pathlib import Path

DUMP_DIR = Path(__file__).parent.parent / "cloud_delivery" / "login_reverse" / "node_modules_dump"

# 关键字（小写匹配）
KEYWORDS = [
    "speed", "vip", "superspeed", "tryspeed", "try_speed", "super_speed",
    "hosthighspeedflow", "vipspeedupurl", "vipteamjoinurl",
    "speedup", "speed-center", "speedcenter", "trial",
    "accelerate", "dcdn", "freedcdn",
]

# 拼接模式：
#   1. 'a' + 'b' + 'c'
#   2. _0x1234 + 'abc' + _0x5678
#   3. "a" + "b"
#   允许空白、换行、反斜杠续行
CONCAT_RE = re.compile(
    r"""([A-Za-z0-9_$"']+(?:\s*\+\s*[A-Za-z0-9_$"']+){1,})""",
    re.DOTALL,
)

# 纯字符串字面量（双引号/单引号/模板字符串片段）
STRING_LITERAL_RE = re.compile(
    r"""(["'])((?:[^\1\\]|\\.)*?)\1""",
    re.DOTALL,
)


def extract_concat_expressions(line: str):
    """从一行中提取所有 + 拼接表达式（至少2个操作数）。"""
    results = []
    for m in CONCAT_RE.finditer(line):
        expr = m.group(1).strip()
        parts = [p.strip() for p in expr.split("+") if p.strip()]
        if len(parts) >= 2:
            results.append((expr, parts))
    return results


def reconstruct(parts):
    """把 ['speed','/','v1','/','xxx'] 拼成 speed/v1/xxx"""
    out = []
    for p in parts:
        if p.startswith(('"', "'")):
            out.append(p[1:-1])
        elif p.startswith("`"):
            # 模板字符串取内容（简化处理）
            out.append(p.strip("`"))
        else:
            # 变量/数字，保留原样但标记
            out.append(p)
    return "".join(out)


def matches_keyword(parts, joined: str) -> bool:
    text = joined.lower()
    return any(k in text for k in KEYWORDS)


def scan_file(path: Path):
    hits = []
    try:
        lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
    except Exception:
        return hits
    for lineno, line in enumerate(lines, 1):
        # 跳过注释行（简化：// 或 /* ）
        stripped = line.strip()
        if stripped.startswith("//") or stripped.startswith("/*") or stripped.startswith("*"):
            continue
        for expr, parts in extract_concat_expressions(line):
            joined = reconstruct(parts)
            if matches_keyword(parts, joined):
                hits.append((lineno, expr, joined, line.strip()))
    return hits


def main():
    parser = argparse.ArgumentParser(description="Deobfuscate speedup strings in xunlei JS dumps")
    parser.add_argument("--dir", type=Path, default=DUMP_DIR, help="node_modules_dump directory")
    parser.add_argument("--output", type=Path, default=None, help="Write markdown report")
    args = parser.parse_args()

    if not args.dir.is_dir():
        print(f"[ERR] directory not found: {args.dir}")
        sys.exit(1)

    js_files = list(args.dir.rglob("*.js"))
    print(f"[INFO] scanning {len(js_files)} JS files in {args.dir}")

    total_hits = 0
    results = []  # (relpath, lineno, expr, joined, raw_line)

    for js in js_files:
        rel = js.relative_to(args.dir)
        hits = scan_file(js)
        for lineno, expr, joined, raw in hits:
            results.append((str(rel), lineno, expr, joined, raw))
            total_hits += 1

    # 按还原字符串排序
    results.sort(key=lambda x: x[3].lower())

    print(f"\n[RESULT] {total_hits} speed-related concatenations found:\n")
    for rel, lineno, expr, joined, raw in results:
        print(f"{rel}:{lineno}")
        print(f"  joined: {joined}")
        print(f"  raw   : {raw[:160]}")
        print()

    if args.output:
        lines = []
        lines.append("# Speedup String Deobfuscation Report\n")
        lines.append(f"- Source: `{args.dir}`")
        lines.append(f"- Files scanned: {len(js_files)}")
        lines.append(f"- Hits: {total_hits}\n")
        lines.append("| File | Line | Reconstructed | Raw snippet |")
        lines.append("|------|------|---------------|-------------|")
        for rel, lineno, expr, joined, raw in results:
            raw_esc = raw.replace("|", "\\|")
            joined_esc = joined.replace("|", "\\|")
            lines.append(f"| `{rel}` | {lineno} | `{joined_esc}` | `{raw_esc[:120]}` |")
        args.output.write_text("\n".join(lines), encoding="utf-8")
        print(f"\n[REPORT] written to {args.output}")


if __name__ == "__main__":
    main()
