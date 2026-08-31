#!/usr/bin/env python3
"""A4-final: 任务状态全字段 dump + RUNNING 即 apply + 配额对比 + 清理."""
import fcntl, json, os, pty, re, select, signal, socket, struct, termios, time
import urllib.request, urllib.error, urllib.parse

ENGINE_DIR = ("/home/z/my-project/repo-smart-downloader/scripts/research/xunlei/"
              "extracted/cross-platform/spk-x64/payload/bin/bin")
LAUNCHER = f"{ENGINE_DIR}/xunlei-pan-cli-launcher.amd64"
WS = os.path.expanduser("~/.nas-engine-test")
LOG = f"{WS}/logs/engine_a4final.log"
OUT = f"{WS}/a4_final.json"
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
    R = {"mode": "a4-final", "started_at": int(time.time())}
    open(LOG, "w").close()
    pid = boot()
    print(f"[+] boot={bool(pid)}", flush=True)
    if not pid:
        R["boot"] = False
        json.dump(R, open(OUT, "w"), ensure_ascii=False, indent=2)
        return
    st, html = http("GET", "/")
    jwt = re.search(rb'uiauth\(value\)\{ return "([^"]+)"', html).group(1).decode()
    H = {"pan-auth": jwt, "Content-Type": "application/json"}

    st, bd = http("GET", f"/drive/v1/tasks?space={urllib.parse.quote(TARGET)}&filters={urllib.parse.quote(json.dumps({'id': {'in': TID}}))}", headers=H)
    task = json.loads(bd)["tasks"][0]
    R["task_full"] = task
    print("[*] task keys:", sorted(task.keys()), flush=True)
    print("[*] phase-ish:", {k: v for k, v in task.items() if k in (
        "phase", "progress", "message", "status", "file_size", "sub_file_index")}, flush=True)
    pp = task.get("params", {})
    print("[*] params keys:", sorted(pp.keys()), flush=True)

    # 轮询 60s，RUNNING 即 apply
    applied = False
    seen = []
    t0 = time.time()
    while time.time() - t0 < 60:
        st, bd = http("GET", f"/drive/v1/tasks?space={urllib.parse.quote(TARGET)}&filters={urllib.parse.quote(json.dumps({'id': {'in': TID}}))}", headers=H, timeout=4)
        if st == 0:
            seen.append(f"t={time.time()-t0:.0f}s HTTP_FAIL {bd[:80]!r}")
            print("    " + seen[-1], flush=True)
            time.sleep(1)
            continue
        t = json.loads(bd)["tasks"][0]
        sig = f"{t.get('phase')}|{t.get('progress')}|{t.get('message')}|{t.get('params',{}).get('speed')}"
        if not seen or seen[-1].split(" ", 1)[-1] != sig:
            line = f"t={time.time()-t0:4.0f}s {sig}"
            seen.append(line)
            print("    " + line, flush=True)
        phase = str(t.get("phase", ""))
        if "RUNNING" in phase and not applied:
            st2, bd2 = http("POST", "/device/v1/try_speed/apply", headers=H, body={}, timeout=8)
            R["apply"] = {"status": st2, "body": bd2[:400].decode("utf-8", "replace"), "at": sig}
            print(f"    >>> APPLY -> {st2} {bd2[:250]!r}", flush=True)
            applied = True
        if "COMPLETE" in phase or "ERROR" in phase or "FAILED" in phase:
            break
        time.sleep(1)
    R["poll"] = seen
    R["applied"] = applied

    st, bd = http("GET", "/device/v1/try_speed/get_info", headers=H)
    R["get_info_after"] = {"status": st, "body": json.loads(bd) if st == 200 else bd[:200].decode("utf-8", "replace")}
    print(f"[*] get_info after -> {st} {bd[:200]!r}", flush=True)

    st, bd = http("DELETE", f"/drive/v1/tasks?space={urllib.parse.quote(TARGET)}&task_ids={TID}", headers=H)
    R["delete"] = {"status": st, "body": bd[:400].decode("utf-8", "replace")}
    print(f"[*] DELETE -> {st} {bd[:250]!r}", flush=True)
    st, bd = http("GET", f"/drive/v1/tasks?space={urllib.parse.quote(TARGET)}&filters={urllib.parse.quote(json.dumps({'id': {'in': TID}}))}", headers=H)
    R["after_delete"] = {"status": st, "body": bd[:300].decode("utf-8", "replace")}
    print(f"[*] verify delete -> {st} {bd[:250]!r}", flush=True)

    R["engine_log_tail"] = open(LOG, errors="replace").read()[-3000:]
    R["finished_at"] = int(time.time())
    json.dump(R, open(OUT, "w"), ensure_ascii=False, indent=2)
    os.kill(pid, signal.SIGTERM)
    print(f"[+] report -> {OUT}")


if __name__ == "__main__":
    main()
