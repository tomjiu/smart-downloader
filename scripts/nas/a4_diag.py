#!/usr/bin/env python3
"""A4-diag: 任务持久化/filters 形态/引擎健康诊断."""
import fcntl, json, os, pty, re, select, signal, socket, struct, termios, time
import urllib.request, urllib.error, urllib.parse

ENGINE_DIR = ("/home/z/my-project/repo-smart-downloader/scripts/research/xunlei/"
              "extracted/cross-platform/spk-x64/payload/bin/bin")
LAUNCHER = f"{ENGINE_DIR}/xunlei-pan-cli-launcher.amd64"
WS = os.path.expanduser("~/.nas-engine-test")
LOG = f"{WS}/logs/engine_a4diag.log"
OUT = f"{WS}/a4_diag.json"
DRIVE = "127.0.0.1:5050"
ROWS, COLS = 40, 120
TARGET = "device_id#c7d089aad73f7e2ddd2c263c2956b5a6"
TID = "VP0KL0GDAslWfb9gld0reITPA1"


def drive_up():
    try:
        with socket.create_connection(("127.0.0.1", 5050), timeout=1):
            return True
    except OSError:
        return False


def http(method, path, headers=None, body=None, timeout=6):
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
                chunk = os.read(master, 65536)
            except OSError:
                chunk = b""
            if chunk:
                with open(LOG, "ab") as f:
                    f.write(chunk)
        if drive_up():
            time.sleep(2)
            return pid
        wpid, _ = os.waitpid(pid, os.WNOHANG)
        if wpid == pid:
            return None
    return None


def main():
    R = {"mode": "a4-diag"}
    open(LOG, "w").close()
    pid = boot()
    R["boot"] = bool(pid)
    print(f"[+] boot={bool(pid)}", flush=True)
    if not pid:
        json.dump(R, open(OUT, "w"), ensure_ascii=False, indent=2)
        return
    st, html = http("GET", "/")
    jwt = re.search(rb'uiauth\(value\)\{ return "([^"]+)"', html).group(1).decode()
    H = {"pan-auth": jwt, "Content-Type": "application/json"}

    probes = [
        ("all-tasks", f"/drive/v1/tasks?space={urllib.parse.quote(TARGET)}&page_token=&limit=50"),
        ("by-id", f"/drive/v1/tasks?space={urllib.parse.quote(TARGET)}&filters={urllib.parse.quote(json.dumps({'id': {'in': TID}}))}"),
        ("type-dl", f"/drive/v1/tasks?space={urllib.parse.quote(TARGET)}&filters={urllib.parse.quote(json.dumps({'type': {'in': 'user#download-url,user#download'}}))}"),
        ("nospace", "/drive/v1/tasks?page_token="),
        ("health", "/device/v1/try_speed/get_info"),
    ]
    for tag, p in probes:
        st, bd = http("GET", p, headers=H)
        body = bd[:500].decode("utf-8", "replace")
        R[tag] = {"status": st, "body": body}
        print(f"[*] {tag:10} -> {st} {body[:260]!r}", flush=True)

    # apply 直接打一次（拿真实错误/回执）
    st, bd = http("POST", "/device/v1/try_speed/apply", headers=H, body={})
    R["apply_empty"] = {"status": st, "body": bd[:300].decode("utf-8", "replace")}
    print(f"[*] apply {{}} -> {st} {bd[:250]!r}", flush=True)
    st, bd = http("POST", "/device/v1/try_speed/apply", headers=H, body={"task_id": TID})
    R["apply_tid"] = {"status": st, "body": bd[:300].decode("utf-8", "replace")}
    print(f"[*] apply task_id -> {st} {bd[:250]!r}", flush=True)
    st, bd = http("POST", "/device/v1/try_speed/apply", headers=H, body={"file_id": TID})
    R["apply_fid"] = {"status": st, "body": bd[:300].decode("utf-8", "replace")}
    print(f"[*] apply file_id -> {st} {bd[:250]!r}", flush=True)

    R["log_tail"] = open(LOG, errors="replace").read()[-2500:]
    json.dump(R, open(OUT, "w"), ensure_ascii=False, indent=2)
    os.kill(pid, signal.SIGTERM)
    print(f"[+] report -> {OUT}")


if __name__ == "__main__":
    main()
