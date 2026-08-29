#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
S2 反编译 C 语料系统扫描 —— 锚点定位 + 函数簇划分（只读语料）
用法: python sweep_s2.py [--json OUT.json]
输出:
  1) 每文件行数 / 函数清单(名, 起止行)
  2) 锚点关键词 -> 行号段分布（功能地图骨架）
  3) 锚点 -> 所在函数 的簇映射（锚点行号 ±150 行归并）
  4) 命中锚点的字符串常量清单（证据原文）
只读语料文件；结果打印到 stdout 并可选存 JSON。
"""
import re
import sys
import json
import os
from collections import defaultdict

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))  # repo root guess
ROOT = r"E:\Code\ai\smart-downloader"
CD = os.path.join(ROOT, "scripts", "research", "cloud_delivery")

FILES = [
    # (短名, 绝对路径)
    ("DownloadSDK",      os.path.join(CD, "phub_line", "xunlei_full_decompiled", "DownloadSDK_DECOMPILED.c")),
    ("P2PFramework",     os.path.join(CD, "phub_line", "xunlei_full_decompiled", "P2PFramework_DECOMPILED.c")),
    ("P2PBase",          os.path.join(CD, "phub_line", "xunlei_full_decompiled", "P2PBase_DECOMPILED.c")),
    ("XUdt",             os.path.join(CD, "phub_line", "xunlei_full_decompiled", "XUdt_DECOMPILED.c")),
    # 聚焦件
    ("dl_encrypt",       os.path.join(CD, "phub_line", "downloadsdk_encrypt.c")),
    ("dl_keyfuncs",      os.path.join(CD, "phub_line", "downloadsdk_key_funcs.c")),
    ("dl_combined",      os.path.join(CD, "phub_line", "downloadsdk_combined.c")),
    ("p2pbase_crypto",   os.path.join(CD, "phub_line", "p2pbase_crypto.c")),
    ("p2pbase_rsa",      os.path.join(CD, "phub_line", "p2pbase_rsa.c")),
    ("p2pbase_aes_core", os.path.join(CD, "phub_line", "p2pbase_aes_core.c")),
    ("p2pf_enc1",        os.path.join(CD, "phub_line", "p2pf_encrypt_funcs.c")),
    ("p2pf_enc2",        os.path.join(CD, "phub_line", "p2pf_encrypt_funcs2.c")),
    ("aes_callers",      os.path.join(CD, "phub_line", "aes_callers_decompiled.c")),
    ("fb_region",        os.path.join(CD, "phub_line", "fb_region_decompiled.c")),
    ("xudt_proto_stack", os.path.join(CD, "phub_line", "xudt_protocol_stack_decompiled.c")),
    ("xudt_addr",        os.path.join(CD, "phub_line", "xudt_addr_decompiled.c")),
    # 云 AI 新交
    ("Server",           os.path.join(CD, "sdk_login_static", "DownloadSDKServer_DECOMPILED.c")),
]

# ---- 任务指定的主锚点（功能地图骨架）----
PRIMARY_ANCHORS = {
    "http":       re.compile(r"https?://", re.I),
    "dotcom":     re.compile(r"\.[Cc][Oo][Mm]\b"),
    "dotnet":     re.compile(r"\.[Nn][Ee][Tt]\b"),
    "url":        re.compile(r"\burl", re.I),
    "login":      re.compile(r"login|signin|sign_in", re.I),
    "token":      re.compile(r"token", re.I),
    "vip":        re.compile(r"\bvip\b|vip[_\-]", re.I),
    "cert":       re.compile(r"cert", re.I),
    "upload":     re.compile(r"upload", re.I),
    "dcdn":       re.compile(r"dcdn", re.I),
    "accelerate": re.compile(r"accelerat", re.I),
    "equity":     re.compile(r"equity", re.I),
    "shub":       re.compile(r"shub", re.I),
    "phub":       re.compile(r"phub", re.I),
    "gcid":       re.compile(r"gcid", re.I),
    "mirror":     re.compile(r"mirror", re.I),
    "resource":   re.compile(r"resource", re.I),
    "report":     re.compile(r"report", re.I),
    "stat":       re.compile(r"\bstat", re.I),
}
# ---- 扩展锚点（抓非 BT/非 PHub 网络行为与加密栈线索）----
EXTENDED_ANCHORS = {
    "crypto":   re.compile(r"\baes\b|\brsa\b|\bmd5\b|sha1|sha256|base64|cipher|encrypt|decrypt|crypt", re.I),
    "host":     re.compile(r"xunlei|sandai|xlmc|xluser|kuaiche|xbase", re.I),
    "netapi":   re.compile(r"socket|WSA|recv|sendto|connect\(|bind\(|htons|inet_addr|gethostby|DNS", 0),
    "httpverb": re.compile(r"\bGET \b|\bPOST \b|User-Agent|Content-Type|HTTP/1", 0),
    "tracker":  re.compile(r"tracker|announce|peer_id|PeerID|magnet|torrent|bittorrent", re.I),
    "account":  re.compile(r"user_?id|session_?id|cookie|passport|device_?id|account", re.I),
    "config":   re.compile(r"config|policy|gray|switch_|abtest", re.I),
    "updver":   re.compile(r"upgrade|update_ver|check_update|new_version|soft.?update", re.I),
    "pay":      re.compile(r"\border\b|\bpay\b|charge|license|trial", re.I),
}

SIG_RE = re.compile(
    r"^[A-Za-z_][A-Za-z0-9_ \t\*]*?[\s\*]((?:FUN|sub)_[0-9A-Fa-f]+|[A-Za-z_]\w*)\s*\(")
HDR_RE = re.compile(r"^//\s*===\s*(.+?)\s*@\s*(0x[0-9A-Fa-f]+)\s*:?\s*(.*)$")
STR_RE = re.compile(r'"([^"\\\n]{3,120})"')

def parse_functions(lines):
    """Ghidra 输出: 签名行(列0含'('无';'), 之后独立'{', 至列0'}'结束。返回函数列表。"""
    funcs = []
    n = len(lines)
    i = 0
    cur_hdr = None
    while i < n:
        line = lines[i]
        m = HDR_RE.match(line.strip())
        if m:
            cur_hdr = (m.group(1), m.group(2), m.group(3))
            i += 1
            continue
        s = SIG_RE.match(line)
        if s and ";" not in line.split("(")[0]:
            name = s.group(1)
            j = i
            # 找函数体开括号（签名可能折行）
            depth_found = False
            k = j
            while k < min(n, j + 12):
                if "{" in lines[k]:
                    depth_found = True
                    break
                if lines[k].rstrip().endswith(";"):  # 只是原型声明
                    break
                k += 1
            if depth_found:
                depth = 0
                e = k
                while e < n:
                    depth += lines[e].count("{") - lines[e].count("}")
                    if depth <= 0 and e >= k:
                        break
                    e += 1
                funcs.append({"name": name, "start": i + 1, "end": e + 1,
                              "hdr": cur_hdr})
                cur_hdr = None
                i = e + 1
                continue
        if not line.startswith("//"):
            pass
        i += 1
    return funcs

def func_at(funcs, ln):
    for f in funcs:
        if f["start"] <= ln <= f["end"]:
            return f
    return None

def scan_file(tag, path):
    if not os.path.exists(path):
        return {"tag": tag, "path": path, "missing": True}
    raw = open(path, "r", encoding="utf-8", errors="replace").read()
    lines = raw.splitlines()
    funcs = parse_functions(lines)
    # 锚点命中
    hits = defaultdict(list)          # anchor -> [line...]
    strings = {}                      # (anchor,line) -> literal
    all_str_hits = []                 # (line, literal, anchors)
    for idx, line in enumerate(lines, 1):
        low_hits = set()
        for aname, rx in PRIMARY_ANCHORS.items():
            if rx.search(line):
                hits[aname].append(idx)
                low_hits.add(aname)
        for aname, rx in EXTENDED_ANCHORS.items():
            if rx.search(line):
                hits["x_" + aname].append(idx)
                low_hits.add("x_" + aname)
        if low_hits:
            for sm in STR_RE.finditer(line):
                lit = sm.group(1)
                all_str_hits.append((idx, lit, sorted(low_hits)))
    return {"tag": tag, "path": path, "size_bytes": os.path.getsize(path),
            "total_lines": len(lines), "num_functions": len(funcs),
            "functions": [{"name": f["name"], "start": f["start"], "end": f["end"],
                           "hdr": f["hdr"][0] if f["hdr"] else None,
                           "addr": f["hdr"][1] if f["hdr"] else None,
                           "note": f["hdr"][2] if f["hdr"] else ""}
                          for f in funcs],
            "hits": dict(hits),
            "string_hits": all_str_hits}

def build_clusters(res, gap=300):
    """把命中按行号排序, 相邻间隔<=gap 归并成簇; 记录簇内锚点/函数/字符串。"""
    events = []
    for aname, lns in res["hits"].items():
        for ln in lns:
            events.append((ln, aname))
    events.sort()
    clusters = []
    for ln, aname in events:
        if clusters and ln - clusters[-1]["last"] <= gap:
            c = clusters[-1]
            c["last"] = max(c["last"], ln)
            c["anchors"].add(aname)
            c["hit_lines"].append(ln)
        else:
            clusters.append({"first": ln, "last": ln,
                             "anchors": {aname}, "hit_lines": [ln]})
    out = []
    fmap = res["functions"]
    import bisect
    starts = [f["start"] for f in fmap]
    for c in clusters:
        fs = set()
        for ln in (c["first"], c["last"], (c["first"]+c["last"])//2):
            i = bisect.bisect_right(starts, ln) - 1
            if i >= 0 and fmap[i]["start"] <= ln <= fmap[i]["end"]:
                fs.add(f"{fmap[i]['name']}@L{fmap[i]['start']}-{fmap[i]['end']}")
        strs = []
        seen = set()
        for ln, lit, _a in res["string_hits"]:
            if c["first"] <= ln <= c["last"] and lit not in seen:
                seen.add(lit)
                strs.append((ln, lit))
        out.append({
            "file": res["tag"],
            "span": f"L{c['first']}-L{c['last']}",
            "n_hits": len(c["hit_lines"]),
            "anchors": sorted(c["anchors"]),
            "functions": sorted(fs),
            "strings": strs[:40],
        })
    return out

def main():
    want_json = "--json" in sys.argv
    out_path = sys.argv[sys.argv.index("--json") + 1] if want_json else None
    if "--funcs" in sys.argv:
        for tag, path in FILES:
            res = scan_file(tag, path)
            if res.get("missing"):
                print(f"[MISS] {tag}")
                continue
            print(f"===== {tag} ({res['total_lines']} lines) =====")
            for f in res["functions"]:
                h = (f["hdr"][0] + " " + f["hdr"][2]) if f["hdr"] else ""
                print(f"{f['start']:>6}-{f['end']:<6} {f['name']}  {h[:100]}")
        return
    report = {}
    for tag, path in FILES:
        res = scan_file(tag, path)
        if res.get("missing"):
            print(f"[MISS] {tag}: {path}")
            report[tag] = res
            continue
        res["clusters"] = build_clusters(res)
        # 压缩行号列表为段
        comp = {}
        for aname, lns in res["hits"].items():
            segs = []
            s = p = lns[0]
            for v in lns[1:]:
                if v - p <= 20:
                    p = v
                else:
                    segs.append(f"{s}-{p}" if p != s else str(s))
                    s = p = v
            segs.append(f"{s}-{p}" if p != s else str(s))
            comp[aname] = segs
        res["hits_compact"] = comp
        del res["string_hits"]
        del res["hits"]
        del res["functions"]
        report[tag] = res
    text = json.dumps(report, ensure_ascii=False, indent=1)
    if want_json:
        with open(out_path, "w", encoding="utf-8") as fh:
            fh.write(text)
        print(f"[OK] written {out_path} ({len(text)} chars)")
    # stdout 摘要
    for tag, res in report.items():
        if res.get("missing"):
            continue
        print(f"\n===== {tag}  lines={res['total_lines']} funcs={res['num_functions']} =====")
        for aname in sorted(res["hits_compact"]):
            segs = res["hits_compact"][aname]
            head = ", ".join(segs[:14])
            more = f" (+{len(segs)-14} segs)" if len(segs) > 14 else ""
            print(f"  {aname:<12}: {head}{more}")

if __name__ == "__main__":
    main()
