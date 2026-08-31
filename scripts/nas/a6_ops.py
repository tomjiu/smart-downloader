#!/usr/bin/env python3
"""A6 ops 托管: 引擎工作区一键冷恢复 + 状态巡检 + 快照清单.

子命令:
  extract    仓库内 SPK -> ~/.nas-engine-test/engine/bin/bin (SHA256 校验)
  envconfig  写 BinDir envconfig (A5 最小配方: 群晖 + ALLOW_CUSTOM_PLATFORM)
  status     工作区巡检 (二进制/哈希/KV/envconfig/凭据)
  snapshot   会话收尾快照 (该 commit 什么/证据在哪些路径/哈希清单) — 防沙盒再丢
用法:
  python3 scripts/nas/a6_ops.py extract
  python3 scripts/nas/a6_ops.py envconfig [--platform 群晖]
  python3 scripts/nas/a6_ops.py status
  python3 scripts/nas/a6_ops.py snapshot > docs/nas/evidence/a6/workspace_snapshot.json
"""
import hashlib, json, os, subprocess, sys, tarfile

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
SPK = os.environ.get("A6_SPK", f"{REPO}/research_bin/nas/spk/nasxunlei-DSM7-x86_64.spk")
WS = os.path.expanduser("~/.nas-engine-test")
ENGINE_BIN = f"{WS}/engine/bin/bin"
BINDIR = f"{WS}/data/.drive/bin"

# ASSET_MANIFEST.md 权威哈希 (3.23.5-0814080017, 2026-08-31 官方源重获)
EXPECT_SHA256 = {
    "xunlei-pan-cli.3.23.5.amd64":
        "fb1fe3401923120bdf47f54a52e8e4c253c6ab11aecc2d89f796074eab12334f",
    "xunlei-pan-cli-launcher.amd64":
        "fb59f7b474f58411a063d2577000ae05bbef0031b699802b21f692c22f212e80",
    "version": None, "version_code": None,
}


def sha256(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def extract():
    if not os.path.exists(SPK):
        sys.exit(f"[!] SPK 不在 {SPK} — 沙盒重建时它已入仓, 先 git pull")
    os.makedirs(ENGINE_BIN, exist_ok=True)
    with tarfile.open(SPK) as t:
        tgz = t.extractfile("package.tgz").read()
    import lzma, io
    with tarfile.open(fileobj=io.BytesIO(lzma.decompress(tgz))) as pt:
        for name in ("xunlei-pan-cli.3.23.5.amd64", "xunlei-pan-cli-launcher.amd64",
                     "version", "version_code"):
            src = pt.extractfile(f"bin/bin/{name}")
            with open(f"{ENGINE_BIN}/{name}", "wb") as out:
                out.write(src.read())
    rep = {}
    for name, expect in EXPECT_SHA256.items():
        got = sha256(f"{ENGINE_BIN}/{name}")
        rep[name] = {"sha256": got, "ok": (expect is None or got == expect)}
        print(f"{'[+]' if rep[name]['ok'] else '[!]'} {name} {got[:16]}...")
    os.chmod(f"{ENGINE_BIN}/xunlei-pan-cli-launcher.amd64", 0o755)
    os.chmod(f"{ENGINE_BIN}/xunlei-pan-cli.3.23.5.amd64", 0o755)
    print(json.dumps(rep, indent=2))
    if not all(v["ok"] for v in rep.values()):
        sys.exit("[!] SHA256 校验失败 — 二进制与 manifest 不符, 停止")
    print(f"[+] 引擎落位 {ENGINE_BIN} (version 3.23.5-0814080017)")


def envconfig(platform="群晖"):
    os.makedirs(BINDIR, exist_ok=True)
    # A5 定案: YAML map[string]string; KEY=VALUE 触发 yaml unmarshal panic
    body = (f'PLATFORM: "{platform}"\n'
            f'OS_VERSION: "geminilake dsm 7.2-64570"\n'
            f'ALLOW_CUSTOM_PLATFORM: "true"\n')
    path = f"{BINDIR}/envconfig"
    open(path, "w").write(body)
    print(f"[+] {path}:\n{body}")
    os.makedirs(f"{WS}/downloads", exist_ok=True)
    print(f"[+] {WS}/downloads 就绪; 拉起前 env 需含 DownloadPipeLimit=10 "
          f"UploadPipeLimit=10 (A4: dump 换算 256)")


def status():
    rep = {"ws": WS, "checks": {}}
    c = rep["checks"]
    c["spk_in_repo"] = os.path.exists(SPK)
    c["engine_bin"] = {n: os.path.exists(f"{ENGINE_BIN}/{n}") for n in EXPECT_SHA256}
    c["envconfig"] = os.path.exists(f"{BINDIR}/envconfig")
    if c["envconfig"]:
        c["envconfig_body"] = open(f"{BINDIR}/envconfig").read()
    for kv in ("drive.bolt", "user.bolt", "task.bolt"):
        p = f"{WS}/data/.drive/{kv}"
        if os.path.exists(p):
            c.setdefault("kv", {})[kv] = os.path.getsize(p)
    c["credentials_hint"] = bool(c.get("kv")) or "冷启动需走 A2 设备码登录链 (scripts/nas/a2_device_flow.py)"
    print(json.dumps(rep, ensure_ascii=False, indent=2))


def snapshot():
    """会话收尾快照: 列出必须入仓的资产与路径, 供 commit 前核对."""
    snap = {
        "rule": "每次会话结束: 新证据 -> docs/nas/evidence/<阶段>/, 新脚本 -> scripts/nas/, "
                "文档更新 -> docs/nas/*.md, 然后 commit+push. 工作区仅存易失物(引擎KV/凭据/日志).",
        "must_commit": [
            "docs/nas/evidence/*/*.json (原始回执, 脱敏 token)",
            "docs/nas/A*_CALIBRATION_FINDINGS.md / A6_PREP_*.md",
            "scripts/nas/*.py",
            "research_bin/nas/spk/*.spk (引擎官方归档, 27MB < 100MB git 限)",
        ],
        "volatile_never_commit": [
            f"{WS}/data/.drive/*.bolt (含凭据)",
            f"{WS}/engine/ (SPK 可由 a6_ops.py extract 重建)",
            "engine 运行日志 (含 device_id/session 痕迹)",
        ],
        "restore_from_scratch": [
            "git clone -b feat/nas-remote-identity https://github.com/mmqz/smart-downloader.git",
            "python3 scripts/nas/a6_ops.py extract",
            "python3 scripts/nas/a6_ops.py envconfig",
            "python3 scripts/nas/a2_device_flow.py  # 冷启动凭据注入 (扫码/设备码)",
            "python3 scripts/nas/a6_probe.py full   # 或先 p6 清 ERROR 任务",
        ],
        "engine_bin_sha256": {
            n: sha256(f"{ENGINE_BIN}/{n}") if os.path.exists(f"{ENGINE_BIN}/{n}") else None
            for n in EXPECT_SHA256},
    }
    print(json.dumps(snap, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "status"
    if cmd == "extract":
        extract()
    elif cmd == "envconfig":
        envconfig(sys.argv[sys.argv.index("--platform") + 1]) if "--platform" in sys.argv else envconfig()
    elif cmd == "status":
        status()
    elif cmd == "snapshot":
        snapshot()
    else:
        __doc__ and print(__doc__)
