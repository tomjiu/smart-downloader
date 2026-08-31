#!/usr/bin/env python3
"""A3-终局: pan-auth header + UIAuth JWT -> 全 API 面校准."""
import fcntl, json, os, pty, re, select, signal, socket, struct, termios, time
import urllib.request, urllib.error

ENGINE_DIR = ("/home/z/my-project/repo-smart-downloader/scripts/research/xunlei/"
              "extracted/cross-platform/spk-x64/payload/bin/bin")
LAUNCHER = f"{ENGINE_DIR}/xunlei-pan-cli-launcher.amd64"
WS = os.path.expanduser("~/.nas-engine-test")
OUT = f"{WS}/a3_result_final.json"
DRIVE = "127.0.0.1:5050"
ROWS, COLS = 40, 120


def drive_up():
    try:
        with socket.create_connection(("127.0.0.1", 5050), timeout=1):
            return True
    except OSError:
        return False


def http(method, path, headers=None, body=None, timeout=12):
    url = f"http://{DRIVE}{path}"
    data = json.dumps(body).encode() if isinstance(body, dict) else body
    req = urllib.request.Request(url, data=data, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()
    except Exception as e:
        return 0, str(e).encode()


def boot():
    env = {k: v for k, v in os.environ.items() if k != "PLATFORM"}
    env.update({
        "DriveListen": DRIVE, "LauncherListen": "127.0.0.1:5051",
        "ConfigPath": f"{WS}/data", "DownloadPATH": f"{WS}/downloads",
        "HOME": f"{WS}/data/.drive", "GIN_MODE": "release",
        "TERM": "xterm", "COLUMNS": str(COLS), "LINES": str(ROWS),
    })
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(ENGINE_DIR)
        os.execve(LAUNCHER, [LAUNCHER, "-pid", f"{WS}/engine.pid"], env)
        os._exit(127)
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    t0 = time.time()
    while time.time() - t0 < 60:
        r, _, _ = select.select([master], [], [], 1.0)
        if r:
            try:
                os.read(master, 65536)
            except OSError:
                pass
        if drive_up():
            time.sleep(2)
            return pid
        wpid, _ = os.waitpid(pid, os.WNOHANG)
        if wpid == pid:
            return None
    return None


def main():
    report = {"mode": "a3-panauth-final", "started_at": int(time.time())}
    pid = boot()
    if not pid:
        report["boot"] = "FAIL"
        json.dump(report, open(OUT, "w"), ensure_ascii=False, indent=2)
        return
    print("[+] engine up", flush=True)

    st, html = http("GET", "/")
    m = re.search(rb'uiauth\(value\)\{ return "([^"]+)"', html)
    jwt = m.group(1).decode() if m else ""
    report["jwt_payload"] = json.loads(__import__("base64").urlsafe_b64decode(jwt.split(".")[1] + "=="))
    print(f"[+] jwt={jwt[:30]}...", flush=True)

    H = {"pan-auth": jwt, "Device-Space": "", "Content-Type": "application/json"}
    st, bd = http("GET", "/drive/v1/tasks?page_token=&filters=", headers=H)
    report["tasks"] = {"status": st, "body": bd[:600].decode("utf-8", "replace")}
    print(f"[*] GET /drive/v1/tasks (pan-auth) -> {st} {bd[:220]!r}", flush=True)

    if st == 200:
        print("\n=== UNLOCKED — 全 API 面校准 ===", flush=True)
        deep = [
            ("GET", "/drive/v1/user/info", None),
            ("GET", "/drive/v1/storage/info", None),
            ("GET", "/drive/v1/events", None),
            ("GET", "/drive/v1/tasks?page_token=", None),
            ("GET", "/device/v1/try_speed/get_info", None),
            ("POST", "/device/v1/try_speed/get_info", {"file_size": 104857600}),
            ("POST", "/device/v1/try_speed/apply", {"file_size": 104857600}),
            ("GET", "/device/v1/info", None),
            ("GET", "/device/v1/config", None),
            ("GET", "/drive/v1/statistics", None),
            ("GET", "/drive/v1/setting", None),
            ("GET", "/drive/v1/history", None),
        ]
        report["deep"] = {}
        for mth, p, b in deep:
            st, bd = http(mth, p, headers=H, body=b)
            report["deep"][f"{mth} {p}"] = {"status": st, "body": bd[:500].decode("utf-8", "replace")}
            print(f"[*] {mth:4} {p:40} -> {st} {bd[:170]!r}", flush=True)

    report["finished_at"] = int(time.time())
    json.dump(report, open(OUT, "w"), ensure_ascii=False, indent=2)
    try:
        os.kill(pid, signal.SIGTERM)
    except Exception:
        pass
    print(f"[+] report -> {OUT}")


if __name__ == "__main__":
    main()
