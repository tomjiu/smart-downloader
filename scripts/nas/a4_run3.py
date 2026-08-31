#!/usr/bin/env python3
"""A4-run3: 常规直链任务 + 持续抢 apply + 配额扣减实证."""
import fcntl, json, os, pty, re, select, signal, socket, struct, termios, time
import urllib.request, urllib.error, urllib.parse

ENGINE_DIR = ("/home/z/my-project/repo-smart-downloader/scripts/research/xunlei/"
              "extracted/cross-platform/spk-x64/payload/bin/bin")
LAUNCHER = f"{ENGINE_DIR}/xunlei-pan-cli-launcher.amd64"
WS = os.path.expanduser("~/.nas-engine-test")
LOG = f"{WS}/logs/engine_a4r3.log"
OUT = f"{WS}/a4_run3.json"
DRIVE = "127.0.0.1:5050"
ROWS, COLS = 40, 120
TARGET = "device_id#c7d089aad73f7e2ddd2c263c2956b5a6"
URL_DL = "https://proof.ovh.net/files/10Mb.dat"
FNAME = "a4r3-10Mb.dat"


def drive_up():
    try:
        with socket.create_connection(("127.0.0.1", 5050), timeout=1):
            return True
    except OSError:
        return False


def http(method, path, headers=None, body=None, timeout=8):
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
    R = {"mode": "a4-run3", "started_at": int(time.time()), "url": URL_DL}
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

    st, bd = http("GET", "/device/v1/try_speed/get_info", headers=H)
    R["quota_before"] = json.loads(bd).get("usage")
    print(f"[*] quota before: {R['quota_before']}", flush=True)

    payload = {"space": TARGET, "type": "user#download-url", "file_size": "0",
               "name": FNAME, "file_name": FNAME, "url": {"url": URL_DL},
               "parent_folder_id": "", "params": {"target": TARGET}}
    st, bd = http("POST", "/drive/v1/task", headers=H, body=payload, timeout=20)
    R["create"] = {"status": st, "body": bd[:500].decode("utf-8", "replace")}
    d = json.loads(bd) if st == 200 else {}
    tid = (d.get("task") or {}).get("id", "")
    R["task_id"] = tid
    print(f"[*] create -> {st} id={tid}", flush=True)
    if not tid:
        json.dump(R, open(OUT, "w"), ensure_ascii=False, indent=2)
        os.kill(pid, signal.SIGTERM)
        return

    filt = urllib.parse.quote(json.dumps({"id": {"in": tid}}))
    q = f"/drive/v1/tasks?space={urllib.parse.quote(TARGET)}&filters={filt}"
    applied, apply_resp, seen = False, None, []
    t0 = time.time()
    while time.time() - t0 < 90:
        st, bd = http("GET", q, headers=H, timeout=5)
        if st != 200:
            sig = f"HTTP_{st}"
        else:
            t = json.loads(bd)["tasks"][0]
            sig = f"{t.get('phase')}|prog={t.get('progress')}|msg={t.get('message')}|{str(t.get('params',{}).get('status'))[:24]}"
            if "RUNNING" in str(t.get("phase", "")) and not applied:
                st2, bd2 = http("POST", "/device/v1/try_speed/apply", headers=H, body={}, timeout=10)
                apply_resp = {"status": st2, "body": bd2[:300].decode("utf-8", "replace")}
                print(f"    >>> APPLY@RUNNING -> {st2} {bd2[:200]!r}", flush=True)
                if "NO_RUNNING" not in bd2.decode("utf-8", "replace"):
                    applied = True
        if not seen or seen[-1].split("  ", 1)[-1] != sig:
            seen.append(f"t={time.time()-t0:4.0f}s  {sig}")
            print("    " + seen[-1], flush=True)
        if "COMPLETE" in sig or "ERROR" in sig or "FAILED" in sig:
            break
        time.sleep(1)
    R["poll"], R["applied"], R["apply"] = seen, applied, apply_resp
    R["final_task"] = t if st == 200 else None

    st, bd = http("GET", "/device/v1/try_speed/get_info", headers=H)
    R["quota_after"] = json.loads(bd).get("usage") if st == 200 else bd[:150].decode("utf-8", "replace")
    print(f"[*] quota after: {R['quota_after']}", flush=True)

    st, bd = http("DELETE", f"/drive/v1/tasks?space={urllib.parse.quote(TARGET)}&task_ids={tid}",
                  headers=H, timeout=25)
    R["delete"] = {"status": st, "body": bd[:300].decode("utf-8", "replace")}
    print(f"[*] DELETE -> {st} {bd[:200]!r}", flush=True)
    st, bd = http("GET", q, headers=H)
    R["after_delete"] = {"status": st, "body": bd[:300].decode("utf-8", "replace")}
    print(f"[*] verify -> {st} {bd[:250]!r}", flush=True)

    R["engine_log_tail"] = open(LOG, errors="replace").read()[-2500:]
    R["finished_at"] = int(time.time())
    json.dump(R, open(OUT, "w"), ensure_ascii=False, indent=2)
    try:
        os.kill(pid, signal.SIGTERM)
    except Exception:
        pass
    print(f"[+] report -> {OUT}")


if __name__ == "__main__":
    main()
