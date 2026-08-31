#!/usr/bin/env python3
"""A2 热启动验证 + #9/#10 校准：正常在线环境启动引擎（无 MITM、无预置），验证凭据 KV 热启动。"""
import fcntl
import json
import os
import pty
import re
import select
import signal
import socket
import struct
import sys
import termios
import time
import urllib.error
import urllib.request

if sys.platform == "win32":
    sys.exit("a2_warmboot_run.py 依赖 Linux-only 模块（fcntl/pty/termios），请于 WSL/Linux 运行")

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
ENGINE = os.environ.get("SD_A2_ENGINE", os.path.normpath(os.path.join(
    SCRIPT_DIR, "..", "research", "xunlei", "extracted", "cross-platform",
    "spk-x64", "payload", "bin", "bin", "xunlei-pan-cli.3.23.5.amd64")))
WS = os.path.expanduser("~/.nas-engine-test")
LOG = f"{WS}/logs/engine_warmboot.log"
OUT = f"{WS}/a2_result_warmboot.json"
DRIVE = "127.0.0.1:5050"
ROWS, COLS = 40, 120
CLIENT_ID = os.environ["SD_XL_CLIENT_ID"]

API_PROBES = [
    ("GET", "/", None),
    ("GET", "/webman/3rdparty/pan-xunlei-com/index.cgi/", None),
    ("GET", "/drive/v1/user/info", None),
    ("GET", "/drive/v1/tasks", None),
    ("GET", "/drive/v1/events", None),
    ("GET", "/device/v1/info", None),
    ("GET", "/device/v1/config", None),
    ("POST", "/device/v1/try_speed/get_info", {}),
]
TRYSPEED_PROBES = [
    ("POST", "/device/v1/try_speed/get_info", {}),
    ("POST", "/device/v1/try_speed/get_info", {"file_size": 104857600}),
    ("GET", "/device/v1/try_speed/get_info", None),
    ("POST", "/device/v1/try_speed/apply", {}),
]


def shape(v, depth=0):
    if depth > 4:
        return "…"
    if isinstance(v, dict):
        return {k: shape(x, depth + 1) for k, x in v.items()}
    if isinstance(v, list):
        return [shape(v[0], depth + 1), f"…({len(v)} items)"] if v else []
    if isinstance(v, str) and len(v) > 12:
        return v[:8] + "…<redacted>"
    return v


def http_req(method, url, headers=None, body=None, timeout=6):
    data = json.dumps(body).encode() if isinstance(body, dict) else None
    req = urllib.request.Request(url, data=data, method=method, headers=headers or {})
    if isinstance(body, dict):
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, _body_shape(r.read(4096).decode("utf-8", "replace"))
    except urllib.error.HTTPError as e:
        return e.code, _body_shape(e.read(4096).decode("utf-8", "replace"))
    except Exception as e:
        return None, f"<conn-error {type(e).__name__}: {e}>"


def _body_shape(raw):
    try:
        return shape(json.loads(raw))
    except Exception:
        return raw[:160]


def drive_up():
    try:
        with socket.create_connection(("127.0.0.1", 5050), timeout=1):
            return True
    except OSError:
        return False


def main():
    report = {"mode": "warmboot-online", "started_at": int(time.time())}
    env = {k: v for k, v in os.environ.items() if k != "PLATFORM"}
    env.update({
        "DriveListen": DRIVE,
        "LauncherListen": "127.0.0.1:5051",
        "ConfigPath": f"{WS}/data",
        "DownloadPATH": f"{WS}/downloads",
        "HOME": f"{WS}/data/.drive",
        "GIN_MODE": "release",
        "TERM": "xterm", "COLUMNS": str(COLS), "LINES": str(ROWS),
    })
    open(LOG, "w").close()
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(os.path.dirname(ENGINE))
        os.execve(ENGINE.replace("xunlei-pan-cli.3.23.5.amd64", "xunlei-pan-cli-launcher.amd64"),
                  ["/home/z/my-project/repo-smart-downloader/scripts/research/xunlei/"
                   "extracted/cross-platform/spk-x64/payload/bin/bin/xunlei-pan-cli-launcher.amd64",
                   "-pid", f"{WS}/engine.pid"], env)
        os._exit(127)
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    print(f"[+] 引擎热启动 pid={pid}（无 MITM 无预置，纯在线）", flush=True)

    up = False
    login_attempted = False
    t0 = time.time()
    while time.time() - t0 < 75:
        r, _, _ = select.select([master], [], [], 1.0)
        if r:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                chunk = b""
            if chunk:
                with open(LOG, "ab") as f:
                    f.write(chunk)
        txt = open(LOG, errors="replace").read()
        if "DoLoginQrcode" in txt:
            login_attempted = True
            print(f"[!] t={time.time()-t0:.0f}s 仍发起扫码登录（热启动失败）", flush=True)
        if drive_up():
            up = True
            print(f"[+] t={time.time()-t0:.0f}s DriveListen 就位！", flush=True)
            time.sleep(2)
            break
        wpid, status = os.waitpid(pid, os.WNOHANG)
        if wpid == pid:
            print(f"[!] 引擎退出 status={status}", flush=True)
            break

    report["drive_listen_up"] = up
    report["login_attempted"] = login_attempted
    txt = open(LOG, errors="replace").read()
    report["login_ok_seen"] = bool(re.search(r"login ok|already_login", txt))
    report["engine_log_tail"] = txt[-4000:]

    if up:
        base = f"http://{DRIVE}"
        report["api_probe"] = {}
        print("\n== #9 API 路由面 ==", flush=True)
        for m, path, body in API_PROBES:
            st, bd = http_req(m, base + path, body=body)
            report["api_probe"][f"{m} {path}"] = {"status": st, "body": bd}
            print(f"[*] {m:4} {path:46} -> {st} {json.dumps(bd, ensure_ascii=False)[:110]}", flush=True)
        print("\n== #10 try_speed 参数面（无 token 头/带 token 头）==", flush=True)
        tok = json.load(open("/home/z/my-project/repo-smart-downloader/scripts/research/xunlei/"
                             "extracted/cross-platform/xllite_token.json"))["response"]
        report["tryspeed"] = {}
        for hs, tag in ((None, "anon"), ({"Authorization": f"Bearer {tok['access_token']}",
                                          "x-client-id": CLIENT_ID}, "auth")):
            for m, path, body in TRYSPEED_PROBES:
                st, bd = http_req(m, base + path, headers=hs, body=body)
                report["tryspeed"][f"{tag} {m} {path} {json.dumps(body)}"] = {"status": st, "body": bd}
                print(f"[*] {tag:4} {m:4} {path:34} -> {st} {json.dumps(bd, ensure_ascii=False)[:120]}", flush=True)

    json.dump(report, open(OUT, "w"), ensure_ascii=False, indent=2)
    try:
        os.kill(pid, signal.SIGTERM)
    except Exception:
        pass
    print(f"\n[+] 报告 -> {OUT}")
    print(f"== 结论 == 热启动: {'成功' if up else '失败'} | 登录门: {'绕过' if up and not login_attempted else '未绕过'}")


if __name__ == "__main__":
    main()
