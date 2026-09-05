#!/usr/bin/env python3
"""state_tests.rs 单文件 → state_tests/ 目录（技术债 #2 第三步，纯移动零语义）。

- 一 mod 一文件：`///doc + #[cfg] + mod xxx { ... }` 整体切割；
  文件即模块：剥 `mod xxx { }` 包装，doc(///→//!) 转文件级文档，
  cfg 属性转 inner（#![cfg(...)]，门控语义等价）。
- mod 之间的外壳项（FakeEngine 家族等）原样归 mod.rs。
- mod.rs = 外壳文档 + use super::* + 间隙项 + 全部 `mod xxx;` 声明。
- 路径不变：state_tests::{tests,bt_alert_tests,...}。
"""

from __future__ import annotations

import re
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SRC = REPO / "crates" / "daemon" / "src" / "state_tests.rs"
DST_DIR = REPO / "crates" / "daemon" / "src" / "state_tests"

text = SRC.read_text()

m_shell = re.search(r"(?m)^use super::\*;$", text)
assert m_shell, "外壳 use super::* 未找到"
head = text[: m_shell.end()]          # 文档 + use super::*
body = text[m_shell.end():]           # mod 项与间隙项

# 仅匹配行首顶层 mod（^ 锚定：嵌套 mod 缩进行不匹配，如 torrent 内的 xunlei_import_tests）
MOD_HEAD_RE = re.compile(r"(?m)^((?:///[^\n]*\n|#!?\[[^\]\n]*\]\n)*)mod (\w+) \{")


def find_block(txt: str, brace: int) -> int:
    depth = 0
    i = brace
    in_str = in_comment = False
    n = len(txt)
    while i < n:
        c = txt[i]
        if in_comment:
            if c == "\n":
                in_comment = False
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
        if c == "/" and i + 1 < n and txt[i + 1] == "/":
            in_comment = True
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
                return i
        i += 1
    raise AssertionError("unbalanced braces")


mods: list[tuple[str, str, str]] = []  # (name, attrs_doc, inner)
gaps: list[str] = []
cur = 0
for m in MOD_HEAD_RE.finditer(body):
    if m.start() > cur:
        gap = body[cur:m.start()]
        if gap.strip():
            gaps.append(gap.rstrip() + "\n")
    name = m.group(2)
    attrs_doc = m.group(1)
    lbrace = body.find("{", m.end() - 1)
    end = find_block(body, lbrace)
    inner = body[lbrace + 1 : end].rstrip() + "\n"
    mods.append((name, attrs_doc, inner))
    cur = end + 1

if body[cur:].strip():
    gaps.append(body[cur:].rstrip() + "\n")

print(f"mods: {len(mods)}, gaps: {len(gaps)}")

DST_DIR.mkdir(exist_ok=True)

for name, attrs_doc, inner in mods:
    # /// doc → //! 文件级文档；#[cfg(..)] → #![cfg(..)] inner（门控整个文件模块，语义等价）
    file_head = []
    for ln in attrs_doc.splitlines():
        s = ln.strip()
        if s.startswith("///"):
            file_head.append("//!" + s[3:])
        elif s.startswith("#["):
            file_head.append("#!" + s[1:])
        elif s == "":
            continue
        else:
            file_head.append(s)
    header = "\n".join(file_head)
    doc = "//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。\n"
    if header:
        content = f"{doc}{header}\n{inner}"
    else:
        content = f"{doc}\n{inner}"
    (DST_DIR / f"{name}.rs").write_text(content)

decls = "\n".join(f"mod {name};" for name, _, _ in mods)
gaps_text = "\n".join(gaps)
mod_rs = f"""{head}
// 技术债 #2 第三步：测试区按 mod 拆分至本目录（一 mod 一文件，纯移动）。
// 路径不变：state_tests::{{tests,bt_alert_tests,...}}；子 mod 的
// `use super::*` 现指向本外壳，glob 解析链与原单文件结构同构。
// 原 mod 间外壳项（FakeEngine 家族等）保留于下方。

{gaps_text}
{decls}
"""
(DST_DIR / "mod.rs").write_text(mod_rs)

SRC.unlink()
print("done: mod.rs + %d files; original removed" % len(mods))
