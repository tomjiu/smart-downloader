#!/usr/bin/env python3
"""A3-B线: 热启动引擎 -> 抓 web UI 全部静态资产 -> 探测 auth 端点面.
进程内完成全部工作(沙盒进程回收对策): 引擎由本脚本拉起、探测后由本脚本收割."""
import fcntl, json, os, pty, re, select, signal, socket, struct, termios, time
import urllib.request, urllib.error

ENGINE_DIR = ("/home/z/my-project/repo-smart-downloader/scripts/research/xunlei/"
              "extracted/cross-platform/spk-x64/payload/bin/bin")
ENGINE = f"{ENGINE_DIR}/xunlei-pan-cli.3.23.5.amd64"
LAUNCHER = f"{ENGINE_DIR}/xunlei-pan-cli-launcher.amd64"
WS = os.path.expanduser("~/.nas-engine-test")
LOG = f"{WS}/logs/engine_a3.log"
OUT = f"{WS}/a3_boot.json"
ASSETS = "/home/z/my-project/download/a3_webui"
DRIVE = "127.0.0.1:5050"
ROWS, COLS = 40, 120


def drive_up():
    try:
        with socket.create_connection(("127.0.0.1", 5050), timeout=1):
            return True
    except OSError:
        return False


def http_raw(method, path, headers=None, body=None, timeout=6):
    """返回 (status, headers_dict, body_bytes)"""
    url = f"http://{DRIVE}{path}"
    data = json.dumps(body).encode() if isinstance(body, dict) else body
    req = urllib.request.Request(url, data=data, method=method,
                                 headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, {k.lower(): v for k, v in r.headers.items()}, r.read()
    except urllib.error.HTTPError as e:
        return e.code, {k.lower(): v for k, v in e.headers.items()}, e.read()
    except Exception as e:
        return 0, {}, str(e).encode()


def main():
    os.makedirs(ASSETS, exist_ok=True)
    report = {"mode": "a3-webui-capture", "started_at": int(time.time())}
    env = {k: v for k, v in os.environ.items() if k != "PLATFORM"}
    env.update({
        "DriveListen": DRIVE, "LauncherListen": "127.0.0.1:5051",
        "ConfigPath": f"{WS}/data", "DownloadPATH": f"{WS}/downloads",
        "HOME": f"{WS}/data/.drive", "GIN_MODE": "release",
        "TERM": "xterm", "COLUMNS": str(COLS), "LINES": str(ROWS),
    })
    open(LOG, "w").close()
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(ENGINE_DIR)
        os.execve(LAUNCHER, [LAUNCHER, "-pid", f"{WS}/engine.pid"], env)
        os._exit(127)
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    print(f"[+] engine pid={pid}", flush=True)

    up = False
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
        if drive_up():
            up = True
            print(f"[+] t={time.time()-t0:.0f}s DriveListen up", flush=True)
            time.sleep(2)
            break
        wpid, status = os.waitpid(pid, os.WNOHANG)
        if wpid == pid:
            print(f"[!] engine exited status={status}", flush=True)
            break
    report["drive_up"] = up
    if not up:
        json.dump(report, open(OUT, "w"), ensure_ascii=False, indent=2)
        try:
            os.kill(pid, signal.SIGTERM)
        except Exception:
            pass
        print("[!] no drive, abort")
        return

    # 1) GET / 的 403 细节（headers 全量）+ 首页 HTML
    st, hd, bd = http_raw("GET", "/")
    report["root"] = {"status": st, "headers": hd, "body_head": bd[:400].decode("utf-8", "replace")}
    open(f"{ASSETS}/index.html", "wb").write(bd)
    print(f"[*] GET / -> {st}, server={hd.get('server')}", flush=True)

    # 2) 403 响应的完整头（看 WWW-Authenticate / set-cookie 等）
    st, hd, bd = http_raw("GET", "/drive/v1/tasks")
    report["tasks_403_headers"] = hd
    report["tasks_403_body"] = bd[:500].decode("utf-8", "replace")
    print(f"[*] GET /drive/v1/tasks -> {st} hdrs={json.dumps(hd)[:200]}", flush=True)

    # 3) 从 HTML 提取静态资源并下载
    html = bd.decode("utf-8", "replace")
    links = set(re.findall(r'(?:src|href)=["\'](/[^"\']+)["\']', html))
    links |= set(re.findall(r'["\'](/assets/[^"\']+)["\']', html))
    report["assets_found"] = sorted(links)
    print(f"[*] assets: {sorted(links)}", flush=True)
    for lnk in sorted(links):
        st, hd, ab = http_raw("GET", lnk, timeout=15)
        name = lnk.strip("/").replace("/", "_") or "root"
        if ab:
            open(f"{ASSETS}/{name}", "wb").write(ab)
        print(f"    {lnk} -> {st} ({len(ab)}B)", flush=True)

    # 4) auth 端点面侦察（常见命名 + 观察状态码差异）
    probes = [
        ("GET", "/device/v1/auth", None),
        ("GET", "/drive/v1/auth", None),
        ("GET", "/auth", None),
        ("GET", "/login", None),
        ("GET", "/device/v1/token", None),
        ("GET", "/drive/v1/user/info", None),
        ("GET", "/drive/v1/storage/info", None),
        ("GET", "/drive/v1/tasks?page_token=&filters=", None),
        ("GET", "/device/v1/info", None),
        ("GET", "/device/v1/config", None),
        ("GET", "/favicon.ico", None),
        ("OPTIONS", "/drive/v1/tasks", None),
    ]
    report["auth_probes"] = {}
    for m, p, b in probes:
        st, hd, bd = http_raw(m, p, body=b)
        report["auth_probes"][f"{m} {p}"] = {
            "status": st, "body": bd[:300].decode("utf-8", "replace")}
        print(f"[*] {m:7} {p:36} -> {st} {bd[:90]!r}", flush=True)

    report["finished_at"] = int(time.time())
    json.dump(report, open(OUT, "w"), ensure_ascii=False, indent=2)
    try:
        os.kill(pid, signal.SIGTERM)
    except Exception:
        pass
    print(f"[+] report -> {OUT}; assets -> {ASSETS}")


if __name__ == "__main__":
    main()
