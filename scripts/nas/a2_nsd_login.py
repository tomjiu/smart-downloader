#!/usr/bin/env python3
"""ns 内执行：lo 拉起 → bind-mount 伪造 hosts → 443 TLS MITM → 引擎登录注入。

在 `unshare -Urnm` 内运行（ns 内 uid=0）。产出：
  - 引擎凭据写入内部 KV（成功标志：KV 文件 mtime 刷新 + auth_token.json 重写）
  - /home/z/.nas-engine-test/ns_login_result.json
"""
import fcntl
import json
import os
import pty
import re
import select
import signal
import socket
import ssl
import struct
import subprocess
import sys
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

WS = os.path.expanduser("~/.nas-engine-test")
ENGINE = os.environ.get("A6_ENGINE_BIN",
          os.path.expanduser("~/.nas-engine-test/engine/bin/bin/xunlei-pan-cli.3.23.5.amd64"))
LOG = f"{WS}/logs/engine_ns.log"
ARCHIVE = os.environ.get("A6_TOKEN_ARCHIVE",
           os.path.expanduser("~/.nas-engine-test/data/.drive/auth_token.json"))
OUT = f"{WS}/ns_login_result.json"
DRIVE = "127.0.0.1:5050"
ROWS, COLS = 40, 120

_tokraw = json.load(open(ARCHIVE))
# 兼容两种落盘形态: 裸 OAuth 响应(a2_device_flow auth_token.json) / {"response":...}(xllite_token.json)
TOKEN_PAYLOAD = _tokraw["response"] if isinstance(_tokraw.get("response"), dict) else _tokraw
hits = {"device_code": 0, "token": 0, "other": 0}


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _json(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        if n:
            self.rfile.read(n)
        if "/auth/device/code" in self.path:
            hits["device_code"] += 1
            self._json(200, {"device_code": "nsfake0001", "user_code": "NSFAKE",
                             "verification_uri_complete": "https://x/?user_code=NSFAKE",
                             "expires_in": 120, "interval": 5})
        elif "/auth/token" in self.path:
            hits["token"] += 1
            self._json(200, TOKEN_PAYLOAD)
        else:
            hits["other"] += 1
            self._json(200, {})

    def do_GET(self):
        hits["other"] += 1
        self._json(200, {})


def up_lo():
    try:
        subprocess.run(["ip", "link", "set", "lo", "up"], check=True)
        return "ip-cmd"
    except Exception:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        name = struct.pack("16s", b"lo")
        buf = bytearray(40)
        r = fcntl.ioctl(s.fileno(), 0x8913, name + bytes(buf))  # SIOCGIFFLAGS
        flags = struct.unpack("H", r[16:18])[0]
        fcntl.ioctl(s.fileno(), 0x8914, name + struct.pack("H", flags | 0x1) + bytes(buf))
        return "ioctl"


def main():
    res = {"mode": "ns-login-injection"}
    res["lo"] = up_lo()
    subprocess.run(["mount", "--bind", "/tmp/a2ns/fake_hosts", "/etc/hosts"], check=True)
    res["hosts"] = open("/etc/hosts").read().count("xunlei")
    print(f"[+] lo up ({res['lo']})；/etc/hosts xunlei 条目 x{res['hosts']}", flush=True)

    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain("/tmp/a2ns/cert.pem", "/tmp/a2ns/key.pem")
    httpd = ThreadingHTTPServer(("0.0.0.0", 443), H)
    httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    print("[+] TLS MITM 就绪 0.0.0.0:443", flush=True)

    env = {k: v for k, v in os.environ.items() if k != "PLATFORM"}
    env.update({
        "DriveListen": DRIVE,
        "LauncherListen": "127.0.0.1:5051",
        "ConfigPath": f"{WS}/data",
        "DownloadPATH": f"{WS}/downloads",
        "HOME": f"{WS}/data/.drive",
        "GIN_MODE": "release",
        "HostXluser": "https://dev-xluser-ssl.xunlei.com",  # 白名单值 + 伪 hosts → 本地
        "SSL_CERT_FILE": "/tmp/a2ns/cert.pem",
        "TERM": "xterm", "COLUMNS": str(COLS), "LINES": str(ROWS),
    })
    open(LOG, "w").close()
    tokp = f"{WS}/data/.drive/auth_token.json"
    kv_before = {f: os.path.getmtime(f"{WS}/data/.drive/{f}")
                 for f in os.listdir(f"{WS}/data/.drive")
                 if os.path.isfile(f"{WS}/data/.drive/{f}")}
    tok_mtime0 = os.path.getmtime(tokp) if os.path.exists(tokp) else 0

    pid, master = pty.fork()
    if pid == 0:
        os.execve(ENGINE, [ENGINE, "-pid", f"{WS}/engine.pid"], env)
        os._exit(127)
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    print(f"[+] 引擎拉起 pid={pid}", flush=True)

    token_ok = False
    t0 = time.time()
    while time.time() - t0 < 90:
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
        if not token_ok and re.search(r"v1/auth/token.*resp_code=200", txt):
            token_ok = True
            print(f"[+] t={time.time()-t0:.0f}s 登录 token 注入成功！等凭据落盘…", flush=True)
            time.sleep(10)  # 给 StartWriteAuthToken / KV 刷盘时间
            break
        wpid, status = os.waitpid(pid, os.WNOHANG)
        if wpid == pid:
            print(f"[!] 引擎退出 status={status}", flush=True)
            break

    res["token_200"] = token_ok
    res["mitm_hits"] = dict(hits)
    res["kv_changed"] = {}
    for f, m0 in kv_before.items():
        m1 = os.path.getmtime(f"{WS}/data/.drive/{f}")
        if m1 != m0:
            res["kv_changed"][f] = True
    tok_mtime1 = os.path.getmtime(tokp) if os.path.exists(tokp) else 0
    res["auth_token_rewritten"] = tok_mtime1 > tok_mtime0
    try:
        os.kill(pid, signal.SIGTERM)
    except Exception:
        pass
    json.dump(res, open(OUT, "w"), ensure_ascii=False, indent=2)
    print(f"[i] 结果: {json.dumps(res, ensure_ascii=False)}", flush=True)
    print("[i] 凭据落盘判定:",
          "成功" if (token_ok and res["kv_changed"]) else "存疑（看 kv_changed）", flush=True)


if __name__ == "__main__":
    main()
