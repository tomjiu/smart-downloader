#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
S1 网页前端业务码扫描（重试版） v2
====================================
语料: scripts/research/cloud_delivery/login_reverse/node_modules_dump/*.js (907 个, ~2.5MB)
基线: docs/research/xunlei/sweep/KNOWN_ITEMS.md  K1~K37
约束: 只读语料; 仅向 docs/research/xunlei/sweep/ 写分析产物(s1_*.json/s1_report.txt),
      最终文档 web_frontend.md 由人工依据本脚本输出撰写。不改任何代码, 不做 git。

v2 变更(针对 dump 实际形态):
  - 语料含 ~76 个 obfuscator 混淆 chunk(_0x 变量+外部字符串表 a2_0x38a1),
    关键串以 'https://ap'+'i-xl9-ssl.'+'xunlei.com' 明文碎片拼接 -> 先做相邻字面量折叠
  - 模板串 ${...} 归一化后再提路径; .get/.post 首参 REST 资源名单独归档
  - 开关值兼容混淆布尔 !0x0/!0x1
分类规则(任务契约):
  SKIP   vendor 启发式: 库指纹高频 且 无 xunlei.com 无中文串(含 \\uXXXX 转义) 无 /v1/
  BIZ    业务候选: 含中文串 或 命中 >=2 个端点
  OTHER  其余(无信号运行时/胶水); 另统计 OBF(混淆)标记供人工复核
"""
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

HERE = Path(__file__).resolve().parent                 # scripts/research/xunlei
DUMP = HERE.parent / "cloud_delivery" / "login_reverse" / "node_modules_dump"
OUT = HERE.parents[2] / "docs" / "research" / "xunlei" / "sweep"

# ---------------------------------------------------------------- 正则 ------
RE_ZH = re.compile(r"[\u4e00-\u9fff][\u4e00-\u9fffA-Za-z0-9_\-·、，。：！？（）()]{0,30}")
RE_ZH_CHAR = re.compile(r"[\u4e00-\u9fff]")
RE_ZH_ESC = re.compile(r"\\u([4-9][0-9a-fA-F]{3})")
RE_URL = re.compile(r"https?://[A-Za-z0-9._\-]+(?::\d+)?(?:/[A-Za-z0-9._~\-/?%=&:+#@!,;$*'()\[\]]*)?")
RE_WS_URL = re.compile(r"wss?://[A-Za-z0-9._\-:/]+")
RE_SCHEME = re.compile(r"\b(xl[a-z0-9+\-.]*://|thunderx?://|magnet:\?|ed2k://)")
RE_PATH_QUOTED = re.compile(r"[\"'](/[A-Za-z0-9._\-]+(?:/[A-Za-z0-9._\-]+)+)[\"']")
RE_PATH_BACKTICK = re.compile(r"[`'](?!/)(?:~{1,4}/)?(/[A-Za-z0-9._\-]+(?:/[A-Za-z0-9._\-]+)+)[`']")
RE_METHOD_CALL = re.compile(r"\.(?:get|post|put|delete|patch|request)\(\s*[\"'`]/")
RE_REST_ARG = re.compile(
    r"\.(?:get|post|put|delete|patch)\(\s*[\"'`]([A-Za-z][A-Za-z0-9_.\-/:]{1,60})[\"'`]")
RE_SWITCH = re.compile(
    r"[\"']([A-Za-z][A-Za-z0-9_]{2,50}(?:[_A-Za-z0-9]*"
    r"(?:[Ss]witch|[Ee]nabl(?:e|ed|es)|flags?|Flags?|[Gg]ray|ab_?[Tt]est|ABTest"
    r"|experiment|Experiment)[A-Za-z0-9_]*))[\"']\s*:\s*"
    r"(true|false|!0x0|!0x1|!0\b|!1\b|null|-?\d+(?:\.\d+)?|\"[^\"\\\n]{0,80}\")")
SEG_LIKE_API = re.compile(
    r"(?:api|v\d|drive|task|web|device|user|member|vip|upload|share|cloud|search)", re.I)
RE_OBF = re.compile(r"_0x[0-9a-f]{4,6}")
# 相邻同引号字面量折叠: 'aa'+'bb' -> 'aabb'
RE_CONCAT = re.compile(r"(['\"])((?:[^'\"\\\n]|\\.){0,160}?)\1\+\1((?:[^'\"\\\n]|\\.){0,160}?)\1")

ASSET_EXT = (".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp", ".ico", ".css",
             ".woff", ".woff2", ".ttf", ".eot", ".mp4", ".mp3", ".webm",
             ".html", ".json")

VENDOR_PATTERNS = [
    ("vue", r"__VUE_HMR_RUNTIME__|_withDirectives|toDisplayString|createElementVNode|__v_isRef|vue-runtime"),
    ("axios", r"isAxiosError|AxiosError|onUploadProgress|xsrftoken|axios"),
    ("core-js", r"__core-js_shared__|core-js|ArrayIteratorPrototypes"),
    ("regenerator", r"regeneratorRuntime|regeneratorDefine"),
    ("element-plus", r"--el-color|el-button|ElMessage|ElInput|el-table__|ElDialog"),
    ("lodash", r"lodash|_baseIteratee|_MapCache"),
    ("babel", r"_classCallCheck|_inheritsLoose|@babel|_objectSpread"),
    ("tslib", r'tslib'),
    ("jsencrypt", r"JSEncrypt|BEGIN PUBLIC KEY"),
    ("protobuf", r"\$protobuf|protobuf\.Reader|protobufjs"),
]
VENDOR_WORDS_PLAIN = ["vue", "axios", "core-js", "regenerator", "element-plus",
                      "lodash", "babel", "webpack", "tslib", "jsencrypt", "protobuf"]


def match_known(host, path):
    """端点级比对 KNOWN_ITEMS A 节(K1~K13)."""
    h = (host or "").lower()
    p = path or ""
    ks = set()
    if "api-gateway-pan.xunlei.com" in h or "/speed-center/" in p:
        ks.add("K2")
    if "api-pan.xunlei.com" in h or re.search(r"/drive/v\d/(?:files|tasks?|file|events|share|privilege|space)\b", p):
        ks.add("K1")
    if "xluser" in h:
        if "/proxy/aliyundrive" in p:
            ks.add("K12")
        elif "/v1/auth/" in p or "/shield/captcha" in p:
            ks.add("K3")
    if h.startswith("speedup.xunlei.com") or re.search(r"/device/v\d/try_speed", p):
        ks.add("K4")
    if not h and re.search(r"/device/v\d/", p):
        ks.add("K30")
    if "dev-speedup" in h:
        ks.add("K5")
    if h.startswith("speed.auth.vip."):
        ks.add("K6")
    if "conf-m-ssl.xunlei.com" in h:
        ks.add("K7")
    if "shoulei" in h:
        ks.add("K8")
    if h.startswith("pan.xunlei.com") and p.startswith("/yc"):
        ks.add("K9")
    if "etl-xlmc" in h:
        ks.add("K10")
    if "lixian-vip" in h or ("lixian" in h and "/download" in p):
        ks.add("K11")
    if "xbase.cloud" in h:
        ks.add("K13")
    return sorted(ks)


FUNC_MARKS = {
    "K14": ["Xqp0", "XW-G4", "XW5Sk", "X9ib", "XVJV"],
    "K21": ["judgeCanTrySpeed", "commitApplyTry", "showPreTryBanner",
            "tryTimeUsagePercentage", "trial_left", "trial_key"],
    "K22": ["VipTeamJoinUrl", "team_times", "teamTaskIDListRef"],
    "K23": ["XL_SetAccelerateCertification", "EquityToken", "AccelerateToken"],
    "K24": ["VipSpeedUpUrl", "superSpeedVipControl", "queryResourceSuperSpeedInfo",
            "checkSpeedUpResult"],
    "K26": ["is_super_speed", "is_try_super_speed", "try_speed"],
}
LOCAL_API_MARKS = ["127.0.0.1", "localhost", ":5050", ":5051", ":21603"]


def unescape_zh(t):
    return RE_ZH_ESC.sub(lambda m: chr(int(m.group(1), 16)), t)


def fold_concat(t):
    """迭代折叠相邻同引号字面量拼接('aa'+'bb'->'aabb'), 还原被拆片的 URL/路径."""
    for _ in range(10):
        n = RE_CONCAT.sub(lambda m: m.group(1) + m.group(2) + m.group(3) + m.group(1), t)
        if n == t:
            break
        t = n
    return t


def scan_file(fp):
    raw = fp.read_text(encoding="utf-8", errors="replace")
    kb = round(len(raw.encode("utf-8", errors="replace")) / 1024, 1)

    esc_n = len(RE_ZH_ESC.findall(raw))
    zh_char_n = len(RE_ZH_CHAR.findall(raw))
    base = unescape_zh(raw) if (esc_n > zh_char_n * 3 and esc_n > 0) else raw
    text = fold_concat(base)                     # 折叠后的"准运行时"文本
    tpl = re.sub(r"\$\{[^}{\n]{0,120}\}", "~", text)   # 模板插值占位化

    zh_hits = RE_ZH.findall(text)
    has_zh = len(RE_ZH_CHAR.findall(text)) >= 3
    is_obf = len(RE_OBF.findall(raw)) >= 10

    lib_votes = sum(1 for _, pat in VENDOR_PATTERNS if re.search(pat, raw))
    plain_hits = sum(raw.count(w) for w in VENDOR_WORDS_PLAIN)
    is_vendor = ((lib_votes >= 2 or plain_hits >= 8)
                 and "xunlei" not in raw and "/v1/" not in raw and not has_zh)

    rel_paths = Counter()
    for src in (tpl, raw):
        for m in RE_PATH_QUOTED.findall(src):
            p = m.rstrip("/")
            low = p.lower()
            if any(low.endswith(e) for e in ASSET_EXT) or len(p) > 100 or "~" in p:
                continue
            segs = [s for s in p.split("/") if s]
            if segs and any(SEG_LIKE_API.search(s) for s in segs):
                rel_paths[p] += 1
        for m in RE_PATH_BACKTICK.finditer(src):
            p = m.group(1).rstrip("/")
            low = p.lower()
            if "${" in p or any(low.endswith(e) for e in ASSET_EXT) or len(p) > 100:
                continue
            segs = [s for s in p.split("/") if s]
            if segs and any(SEG_LIKE_API.search(s) for s in segs):
                rel_paths[p] += 1

    urls = Counter(u.split("\\")[0].rstrip(".,;")
                   for u in (m.group(0) for m in RE_URL.finditer(tpl)))
    ws = Counter(RE_WS_URL.findall(text))
    schemes = Counter(m.lower() for m in RE_SCHEME.findall(text))
    rest_args = Counter(m for m in RE_REST_ARG.findall(text)
                        if not m.lower().startswith(("http", "//")))
    method_calls = len(RE_METHOD_CALL.findall(tpl))

    switches = []
    seen_sw = set()
    for k, v in RE_SWITCH.findall(tpl):
        if k not in seen_sw:
            seen_sw.add(k)
            switches.append((k, v.replace("!0x0", "true").replace("!0x1", "false")))

    uniq_eps = set(rel_paths) | set(urls)
    n_eps = len(set(rel_paths)) + len(set(urls))
    marks = sorted({k for k, words in FUNC_MARKS.items() if any(w in raw for w in words)})
    local_marks = [m for m in LOCAL_API_MARKS if m in raw]
    has_xunlei = "xunlei.com" in raw

    score = 0
    if has_xunlei:
        score += 30
    score += min(30, len(zh_hits) // 2)
    if "/v1/" in text:
        score += 10
    if "/v2/" in text:
        score += 5
    if "axios.create" in text:
        score += 10
    if "client_id" in text:
        score += 5
    score += min(15, method_calls * 3)
    score += min(12, n_eps * 4)
    score += min(10, len(zh_hits))
    if is_obf:
        score += 15                               # 混淆本身就是业务保护信号

    return {
        "name": fp.name, "kb": kb, "is_vendor": is_vendor,
        "is_biz": bool(has_zh or n_eps >= 2), "score": score,
        "zh_n": len(zh_hits), "n_eps": n_eps, "is_obf": is_obf,
        "has_xunlei": has_xunlei,
        "zh_sample": list(dict.fromkeys(zh_hits))[:40],
        "rel_paths": rel_paths, "urls": urls, "ws": ws, "schemes": schemes,
        "rest_args": rest_args, "switches": switches,
        "marks": marks, "local_marks": local_marks,
        "method_calls": method_calls,
    }


def main():
    files = sorted(DUMP.glob("*.js"))
    recs = []
    ep_paths, ep_urls, ep_ws, ep_schemes = {}, {}, {}, {}
    switches_by_file, rest_by_res = {}, {}

    for fp in files:
        r = scan_file(fp)
        recs.append(r)
        fn = r["name"]
        if r["is_vendor"]:
            continue
        for p, c in r["rel_paths"].items():
            rec = ep_paths.setdefault(p, {"count": 0, "files": {}, "known": []})
            rec["count"] += c
            rec["files"][fn] = rec["files"].get(fn, 0) + c
            ks = match_known(None, p)
            if ks:
                rec["known"] = sorted(set(rec["known"]) | set(ks))
        for u, c in r["urls"].items():
            hm = re.match(r"https?://([^/]+)(/.*)?", u)
            host, path = (hm.group(1), hm.group(2) or "") if hm else ("", "")
            rec = ep_urls.setdefault(u, {"count": 0, "host": host, "files": {}, "known": []})
            rec["count"] += c
            rec["files"][fn] = rec["files"].get(fn, 0) + c
            ks = match_known(host, path)
            if ks:
                rec["known"] = sorted(set(rec["known"]) | set(ks))
        for w, c in r["ws"].items():
            rec = ep_ws.setdefault(w, {"count": 0, "files": {}})
            rec["count"] += c
            rec["files"][fn] = rec["files"].get(fn, 0) + c
        for s_, c in r["schemes"].items():
            rec = ep_schemes.setdefault(s_, {"count": 0, "files": {}})
            rec["count"] += c
            rec["files"][fn] = rec["files"].get(fn, 0) + c
        if r["switches"]:
            cur = switches_by_file.setdefault(fn, {})
            cur.update(dict(r["switches"]))
        for res, c in r["rest_args"].items():
            rec = rest_by_res.setdefault(res, {"count": 0, "files": {}})
            rec["count"] += c
            rec["files"][fn] = rec["files"].get(fn, 0) + c

    vendors = [r for r in recs if r["is_vendor"]]
    biz = [r for r in recs if r["is_biz"] and not r["is_vendor"]]
    others = [r for r in recs if not r["is_vendor"] and not r["is_biz"]]
    biz.sort(key=lambda r: (-r["score"], -r["kb"]))
    top15 = biz[:15]
    OUT.mkdir(parents=True, exist_ok=True)

    inv = [{"file": r["name"], "kb": r["kb"],
            "class": "SKIP" if r["is_vendor"] else ("BIZ" if r["is_biz"] else "OTHER"),
            "score": r["score"], "zh": r["zh_n"], "eps": r["n_eps"],
            "obf": r["is_obf"], "marks": r["marks"]} for r in recs]
    (OUT / "s1_inventory.json").write_text(json.dumps(inv, ensure_ascii=False, indent=1), encoding="utf-8")
    (OUT / "s1_endpoints.json").write_text(json.dumps({
        "rel_paths": {k: v for k, v in sorted(ep_paths.items(), key=lambda kv: -kv[1]["count"])},
        "urls": {k: v for k, v in sorted(ep_urls.items(), key=lambda kv: -kv[1]["count"])},
        "websocket": ep_ws, "schemes": ep_schemes,
        "rest_args": {k: v for k, v in sorted(rest_by_res.items(), key=lambda kv: -kv[1]["count"])},
    }, ensure_ascii=False, indent=1), encoding="utf-8")
    (OUT / "s1_switches.json").write_text(
        json.dumps(switches_by_file, ensure_ascii=False, indent=1), encoding="utf-8")

    L = []
    L.append(f"TOTAL={len(recs)} SKIP(vendor)={len(vendors)} BIZ={len(biz)} OTHER={len(others)} "
             f"(OBF files total={sum(1 for r in recs if r['is_obf'])}, "
             f"OBF in BIZ={sum(1 for r in biz if r['is_obf'])})")
    L.append(f"unique_rel_paths={len(ep_paths)} unique_urls={len(ep_urls)} ws={len(ep_ws)} "
             f"schemes={len(ep_schemes)} rest_args={len(rest_by_res)} "
             f"switch_keys={sum(len(v) for v in switches_by_file.values())}")

    L.append("")
    L.append("== TOP20 BIZ ==")
    for r in top15 + biz[15:20]:
        L.append(f"{r['name']}\t{r['kb']}KB\tscore={r['score']}\tzh={r['zh_n']}\teps={r['n_eps']}"
                 f"\tobf={int(r['is_obf'])}\tmarks={','.join(r['marks']) or '-'}"
                 f"\tlocal={','.join(r['local_marks']) or '-'}")

    L.append("")
    L.append("== FUNC_MARK / LOCAL HITS ==")
    for r in recs:
        if r["marks"] or r["local_marks"]:
            L.append(f"{r['name']}\tmarks={','.join(r['marks']) or '-'}"
                     f"\tlocal={','.join(r['local_marks']) or '-'}")

    L.append("")
    L.append(f"== REL PATHS 全表({len(ep_paths)}) ==")
    for p, v in sorted(ep_paths.items(), key=lambda kv: -kv[1]["count"]):
        L.append(f"{v['count']}\t{p}\t[{','.join(list(v['files'])[:6])}]\t{','.join(v['known']) or '-'}")

    L.append("")
    L.append(f"== ABSOLUTE URLS 全表({len(ep_urls)}) ==")
    for u, v in sorted(ep_urls.items(), key=lambda kv: -kv[1]["count"]):
        L.append(f"{v['count']}\t{u[:140]}\t[{','.join(list(v['files'])[:4])}]\t{','.join(v['known']) or '-'}")

    if ep_ws or ep_schemes:
        L.append("")
        L.append("== WS / SCHEME ==")
        for w, v in ep_ws.items():
            L.append(f"WS\t{w}\t{v['count']}\t{','.join(list(v['files'])[:4])}")
        for s_, v in ep_schemes.items():
            L.append(f"SCH\t{s_}\t{v['count']}\t{','.join(list(v['files'])[:4])}")

    L.append("")
    L.append(f"== REST ARGS (.get/.post 首参) Top120 / 共{len(rest_by_res)} ==")
    for res, v in sorted(rest_by_res.items(), key=lambda kv: -kv[1]["count"])[:120]:
        L.append(f"{v['count']}\t{res}\t[{','.join(list(v['files'])[:5])}]")

    L.append("")
    L.append("== SWITCH KEYS ==")
    agg = {}
    for fn, sw in switches_by_file.items():
        for k, v in sw.items():
            agg.setdefault(k, []).append((fn, v))
    for k in sorted(agg):
        ex_fn, ex_v = agg[k][0]
        L.append(f"{k}\t={ex_v}\tfiles={','.join(sorted(f for f, _ in agg[k])[:5])}")

    L.append("")
    L.append("== TOP35 BIZ 中文样本 ==")
    for r in biz[:35]:
        if r["zh_sample"]:
            tag = "(OBF)" if r["is_obf"] else ""
            L.append(f"[{r['name']}{tag}] " + " | ".join(r["zh_sample"][:20]))

    L.append("")
    L.append("== OTHER 抽样(前25个名字+KB, 供复核 vendor 性质) ==")
    for r in others[:25]:
        L.append(f"{r['name']}\t{r['kb']}KB\tobf={int(r['is_obf'])}")

    (OUT / "s1_report.txt").write_text("\n".join(L), encoding="utf-8")
    print(f"DONE total={len(recs)} skip={len(vendors)} biz={len(biz)} other={len(others)} "
          f"obf_total={sum(1 for r in recs if r['is_obf'])} "
          f"rel_paths={len(ep_paths)} urls={len(ep_urls)} rest_args={len(rest_by_res)} "
          f"ws={len(ep_ws)} schemes={len(ep_schemes)} "
          f"switch_keys={sum(len(v) for v in switches_by_file.values())}")
    print("top15:", ",".join(r["name"] for r in top15))


if __name__ == "__main__":
    main()
