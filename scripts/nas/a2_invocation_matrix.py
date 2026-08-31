#!/usr/bin/env python3
"""调用姿势矩阵测试：run 前置 / run 后置 / launcher 透传，各 12s 判定。"""
import fcntl
import os
import pty
import select
import signal
import struct
import termios
import time

BINDIR = ("/home/z/my-project/repo-smart-downloader/scripts/research/xunlei/"
          "extracted/cross-platform/spk-x64/payload/bin/bin")
ENGINE = f"{BINDIR}/xunlei-pan-cli.3.23.5.amd64"
LAUNCHER = f"{BINDIR}/xunlei-pan-cli-launcher.amd64"
WS = os.path.expanduser("~/.nas-engine-test")

VARIANTS = [
    ("run-first", [ENGINE, "run", "-pid", f"{WS}/engine.pid"]),
    ("pid-first-run-last", [ENGINE, "-pid", f"{WS}/engine.pid", "run"]),
    ("launcher", [LAUNCHER, "-pid", f"{WS}/engine.pid"]),
]

BASE_ENV = {
    "DriveListen": "127.0.0.1:5050", "LauncherListen": "127.0.0.1:5051",
    "ConfigPath": f"{WS}/data", "DownloadPATH": f"{WS}/downloads",
    "HOME": f"{WS}/data/.drive", "GIN_MODE": "release",
    "TERM": "xterm", "COLUMNS": "120", "LINES": "40",
}

for name, argv in VARIANTS:
    env = {k: v for k, v in os.environ.items() if k != "PLATFORM"}
    env.update(BASE_ENV)
    log = f"{WS}/logs/inv_{name}.log"
    open(log, "w").close()
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(BINDIR)
        os.execve(argv[0], argv, env)
        os._exit(127)
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    t0 = time.time()
    while time.time() - t0 < 12:
        r, _, _ = select.select([master], [], [], 1.0)
        if r:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                chunk = b""
            if chunk:
                open(log, "ab").write(chunk)
        wpid, status = os.waitpid(pid, os.WNOHANG)
        if wpid == pid:
            break
    try:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, os.WNOHANG)
    except Exception:
        pass
    txt = open(log, errors="replace").read()
    verdict = []
    if "app.Before start" in txt:
        verdict.append("startService ✓")
    if "NAME:" in txt:
        verdict.append("help打印✗")
    if "DoLoginQrcode" in txt:
        verdict.append("扫码请求✗(未热)")
    if "already_login" in txt or "login ok" in txt:
        verdict.append("已登录✓")
    print(f"[{name:18}] {' | '.join(verdict) or '无信号'}")
