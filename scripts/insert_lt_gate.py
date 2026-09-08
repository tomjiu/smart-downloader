#!/usr/bin/env python3
"""向会创建 libtorrent session 的测试幂等插入 LT_SESSION_GATE 串行门。

背景：libtorrent 2.0 session_params 默认监听 0.0.0.0:6881，ffi 无 settings
导出无法改端口；cargo test 同一 test binary 内多线程并行 → 多 session 抢
6881 → flaky。方案：进程内 tokio Mutex 门（crates/daemon/tests/common/
lt_gate.rs），测试体首行持锁直至用例结束（session 全生命周期被门覆盖）。

判定规则（保守的函数级数据流）：
1. 污染源 = 函数体直接含 `BtEngine::new`（serve 类 helper）。
2. 传播 = 函数体调用了任何污染函数（迭代到不动点）→ helper 的调用链上
   的测试函数全部污染。
3. 污染的 `#[tokio::test]` → 插 `.lock().await` 门；污染的 `#[test]` →
   插 `.blocking_lock()` 门（tokio Mutex 原生支持阻塞获取）。
4. 幂等：函数体首部已引用 `lt_gate` 则跳过该函数；已带 `mod common;` 则
   不重复添加；common/mod.rs 已声明 `pub mod lt_gate;` 则不动。

用法：
    python3 scripts/insert_lt_gate.py --dry-run   # 只报告
    python3 scripts/insert_lt_gate.py             # 实际写入
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TESTS_DIR = REPO / "crates" / "daemon" / "tests"
COMMON_MOD = TESTS_DIR / "common" / "mod.rs"

GATE_ASYNC = "let _lt = crate::common::lt_gate::LT_SESSION_GATE.lock().await;"
GATE_SYNC = "let _lt = crate::common::lt_gate::LT_SESSION_GATE.blocking_lock;"
MOD_COMMON_DECL = "mod common;"

FN_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)", re.M)
ATTR_RE = re.compile(r"#\[[^\[\]]*\]")


class Fn:
    __slots__ = ("name", "body_start", "body_end", "async_", "test_kind", "indent")

    def __init__(self, name, body_start, body_end, async_, test_kind, indent):
        self.name = name
        self.body_start = body_start  # offset of '{'
        self.body_end = body_end      # offset of matching '}'
        self.async_ = async_
        self.test_kind = test_kind    # None | "tokio" | "sync"
        self.indent = indent


def scan_functions(text: str) -> list[Fn]:
    """定位全部顶层/嵌套函数：名称、体区间、async、test 属性、缩进。"""
    fns = []
    for m in FN_RE.finditer(text):
        name = m.group(1)
        line_start = text.rfind("\n", 0, m.start()) + 1
        indent = len(m.group(0)) - len(m.group(0).lstrip())
        # 向上收集连续属性行（#[...]，允许带缩进）
        test_kind = None
        probe = text[: m.start()]
        lines = probe.split("\n")
        attr_blob = ""
        for ln in reversed(lines):
            s = ln.strip()
            if s == "":
                continue  # fn 行残留 / 属性行间空行
            if s.startswith("#["):
                attr_blob += s
            else:
                break
        if "#[tokio::test" in attr_blob:
            test_kind = "tokio"
        elif "#[test]" in attr_blob:
            test_kind = "sync"
        # async 判定
        async_ = bool(re.search(r"\basync\s+fn\b", m.group(0)))
        # 找函数体 '{'：从 m.end() 起第一个 '{'（签名中不会有 '{'，除非泛型/where——测试代码无）
        brace = text.find("{", m.end())
        if brace == -1:
            continue
        depth = 0
        i = brace
        n = len(text)
        in_str = False
        in_line_comment = False
        while i < n:
            c = text[i]
            if in_line_comment:
                if c == "\n":
                    in_line_comment = False
                i += 1
                continue
            if in_str:
                if c == "\\":
                    i += 2
                    continue
                if c == '"':
                    in_str = False
                i += 1
                continue
            if c == "/" and i + 1 < n and text[i + 1] == "/":
                in_line_comment = True
                i += 2
                continue
            if c == '"':
                in_str = True
                i += 1
                continue
            if c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    fns.append(Fn(name, brace, i, async_, test_kind, indent))
                    break
            i += 1
    return fns


def body_text(text: str, fn: Fn) -> str:
    return text[fn.body_start + 1 : fn.body_end]


def is_polluted(text: str, fns: list[Fn]) -> set[str]:
    """污染源（BtEngine::new）+ 调用传播到不动点。"""
    polluted = {f.name for f in fns if "BtEngine::new" in body_text(text, f)}
    changed = True
    while changed:
        changed = False
        for f in fns:
            if f.name in polluted:
                continue
            b = body_text(text, f)
            for p in polluted:
                if re.search(r"\b" + re.escape(p) + r"\s*\(", b):
                    polluted.add(f.name)
                    changed = True
                    break
    return polluted


def first_stmt_indent(text: str, fn: Fn) -> tuple[int, int]:
    """返回 (插入 offset, 缩进)。插入点 = 函数体 '{' 后首条语句前。"""
    inner = text[fn.body_start + 1 : fn.body_end]
    m = re.search(r"\S", inner, re.S)
    if not m:
        # 空函数体：{ 紧后插入
        off = fn.body_start + 1
        return off, fn.indent + 4
    stmt_off = fn.body_start + 1 + m.start()
    # 该语句所在行的行首缩进
    line_start = text.rfind("\n", 0, stmt_off) + 1
    ws = re.match(r"[ \t]*", text[line_start:]).group(0)
    return stmt_off, len(ws)


def insert_gate(text: str, fns: list[Fn], polluted: set[str], report: dict) -> str:
    """从文件尾向前插入（offset 不失效）。"""
    inserts = []
    for f in fns:
        if f.name not in polluted:
            continue
        if f.test_kind is None:
            continue
        b = body_text(text, f)
        if "lt_gate" in b[:240]:
            report.setdefault("skipped", []).append(f.name)
            continue
        gate = GATE_ASYNC if f.test_kind == "tokio" else GATE_SYNC
        off, ind = first_stmt_indent(text, f)
        inserts.append((off, " " * ind + gate + "\n"))
        report.setdefault("inserted", []).append(
            f"{f.name}({'async' if f.test_kind == 'tokio' else 'sync'})"
        )
    for off, s in sorted(inserts, key=lambda x: -x[0]):
        text = text[:off] + s + text[off:]
    return text


def ensure_mod_common(text: str) -> tuple[str, bool]:
    if re.search(r"^\s*mod\s+common\s*;", text, re.M):
        return text, False
    # 插在 inner attribute 块之后
    m = None
    for m in ATTR_RE.finditer(text):
        if text.find("\n", m.end()) != -1 and m.start() == 0:
            continue
        break
    lines = text.split("\n")
    ins_at = 0
    for i, ln in enumerate(lines):
        s = ln.strip()
        if s.startswith("//!") or s.startswith("#!"):
            ins_at = i + 1  # inner doc / inner attr 均属文件头
        elif s == "" and ins_at == i:
            ins_at = i + 1  # 头部内的空行
        else:
            break  # 首个 item（use/mod/fn…）——插入点在此之前
    lines.insert(ins_at, MOD_COMMON_DECL)
    return "\n".join(lines), True


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    files = sorted(p for p in TESTS_DIR.glob("*.rs"))
    total_inserted = 0
    for path in files:
        text = path.read_text()
        fns = scan_functions(text)
        polluted = is_polluted(text, fns)
        report: dict = {}
        new_text = insert_gate(text, fns, polluted, report)
        added_mod = False
        if report.get("inserted"):
            new_text, added_mod = ensure_mod_common(new_text)
        n = len(report.get("inserted", []))
        total_inserted += n
        if n or added_mod:
            print(f"{path.name}: insert {n}, mod_common {'+1' if added_mod else 'ok'}")
            if report.get("inserted"):
                print("  + " + ", ".join(report["inserted"]))
            if report.get("skipped"):
                print("  = already gated: " + ", ".join(report["skipped"]))
            if not args.dry_run:
                path.write_text(new_text)
        else:
            print(f"{path.name}: clean")

    # common/mod.rs 挂载 lt_gate
    cm = COMMON_MOD.read_text()
    if "pub mod lt_gate;" not in cm:
        if not args.dry_run:
            COMMON_MOD.write_text(cm.replace(
                "//! M6 测试共享：直链 HTTP server（daemon 任务下载源）。\n",
                "//! M6 测试共享：直链 HTTP server（daemon 任务下载源）。\n\npub mod lt_gate;\n",
                1,
            ))
        print("common/mod.rs: +pub mod lt_gate;")
    print(f"\nTOTAL inserted: {total_inserted} ({'DRY-RUN' if args.dry_run else 'WRITTEN'})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
