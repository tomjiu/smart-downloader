#!/usr/bin/env python3
"""A4-run4: 提升 RLIMIT_NOFILE 重跑任务 + 监控引擎 Threads/FD 排查 90120."""
import fcntl, json, os, pty, re, resource, select, signal, socket, struct, termios, time
import urllib.request, urllib.error, urllib.parse

ENGINE_DIR = ("/home/z/my-project/repo-smart-downloader/scripts/research/xunlei/"
              "extracted/cross-platform/spk-x64/payload/bin/bin")
LAUNCHER = f"{ENGINE_DIR}/xunlei-pan-cli-launcher.amd64"
WS = os.path.expanduser("~/.nas-engine-test")
LOG = f"{WS}/logs/engine_a4r4.log"
OUT = f"{WS}/a4_run4.json"
DRIVE = "127.0.0.1:5050"
ROWS, COLS = 40, 120
TARGET = "device_id#c7d089aad73f7e2ddd2c263c2956b5a6"
URL_DL = "https://proof.ovh.net/files/10Mb.dat"


def drive_up():
    try:
        with socket.create_connection(("127.0.0.1", 5050), timeout=1):
            return True
    except OSError:
        return False


def http(method, path, headers=None, body=None, timeout=8):
    data = json.dumps(body).encode() if isinstance(body, dict) else body
    req = urllib.request.Request(f"http://{DRIVE}{path}", data=data, method=method, headers=headers or {})
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
        # 子进程：提升 fd 限制
        try:
            resource.setrlimit(resource.RLIMIT_NOFILE, (65536, 100000))
        except Exception:
            pass
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


def engine_stats(pid):
    """真实引擎 pid（launcher 的子进程）的线程/fd 统计"""
    out = []
    for p in (pid,):
        try:
            kids = os.listdir(f"/proc/{p}/task")
        except Exception:
            continue
        for d in os.listdir("/proc"):
            if not d.isdigit():
                continue
            try:
                with open(f"/proc/{d}/status") as f:
                    s = f.read()
                if f"PPid:\t{p}" in s:
                    out.append((int(d), re.search(r"Threads:\t(\d+)", s).group(1)))
            except Exception:
                pass
    return out


def stats_deep():
    """找 xunlei-pan-cli 主进程"""
    res = []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            cmd = open(f"/proc/{d}/cmdline", "rb").read().decode("utf-8", "replace")
            if "xunlei-pan-cli.3.23.5" in cmd:
                nthreads = int(re.search(r"Threads:\t(\d+)", open(f"/proc/{d}/status").read()).group(1))
                nfds = len(os.listdir(f"/proc/{d}/fd"))
                res.append({"pid": int(d), "threads": nthreads, "fds": nfds})
        except Exception:
            pass
    return res


def main():
    R = {"mode": "a4-run4-nofile", "started_at": int(time.time())}
    open(LOG, "w").close()
    pid = boot()
    print(f"[+] boot={bool(pid)}", flush=True)
    if not pid:
        R["boot"] = False
        json.dump(R, open(OUT, "w"), ensure_ascii=False, indent=2)
        return
    R["engine_stats_boot"] = stats_deep()
    print(f"[*] engine stats @boot: {R['engine_stats_boot']}", flush=True)

    st, html = http("GET", "/")
    jwt = re.search(rb'uiauth\(value\)\{ return "([^"]+)"', html).group(1).decode()
    H = {"pan-auth": jwt, "Content-Type": "application/json"}

    payload = {"space": TARGET, "type": "user#download-url", "file_size": "0",
               "name": "a4r4.dat", "file_name": "a4r4.dat",
               "url": {"url": URL_DL}, "parent_folder_id": "",
               "params": {"target": TARGET}}
    st, bd = http("POST", "/drive/v1/task", headers=H, body=payload, timeout=20)
    tid = (json.loads(bd).get("task") or {}).get("id", "") if st == 200 else ""
    R["create"] = {"status": st, "task_id": tid, "body": bd[:300].decode("utf-8", "replace")}
    print(f"[*] create -> {st} id={tid}", flush=True)

    seen, applied = [], False
    q = f"/drive/v1/tasks?space={urllib.parse.quote(TARGET)}&filters={urllib.parse.quote(json.dumps({'id': {'in': tid}}))}"
    t0 = time.time()
    while time.time() - t0 < 75:
        st, bd = http("GET", q, headers=H, timeout=5)
        if st == 200:
            t = json.loads(bd)["tasks"][0]
            sig = f"{t.get('phase')}|{t.get('message')}|{str(t.get('params',{}).get('error'))[:30]}"
        else:
            sig = f"HTTP_{st}"
        if not seen or seen[-1].split("  ", 1)[-1] != sig:
            stats = stats_deep()
            line = f"t={time.time()-t0:4.0f}s  {sig}  stats={stats}"
            seen.append(line)
            print("    " + line, flush=True)
        if "RUNNING" in sig and not applied:
            st2, bd2 = http("POST", "/device/v1/try_speed/apply", headers=H, body={}, timeout=10)
            R["apply"] = {"status": st2, "body": bd2[:200].decode("utf-8", "replace")}
            print(f"    APPLY -> {st2} {bd2[:150]!r}", flush=True)
            applied = True
        if "ERROR" in sig or "COMPLETE" in sig:
            break
        time.sleep(1)
    R["poll"], R["applied"] = seen, applied

    st, bd = http("GET", "/device/v1/try_speed/get_info", headers=H)
    R["quota_after"] = json.loads(bd).get("usage") if st == 200 else None
    print(f"[*] quota after: {R['quota_after']}", flush=True)

    st, bd = http("DELETE", f"/drive/v1/tasks?space={urllib.parse.quote(TARGET)}&task_ids={tid}",
                  headers=H, timeout=25)
    R["delete"] = {"status": st}
    print(f"[*] DELETE -> {st}", flush=True)
    R["engine_log_tail"] = open(LOG, errors="replace").read()[-2000:]
    R["finished_at"] = int(time.time())
    json.dump(R, open(OUT, "w"), ensure_ascii=False, indent=2)
    try:
        os.kill(pid, signal.SIGTERM)
    except Exception:
        pass
    print(f"[+] report -> {OUT}")


if __name__ == "__main__":
    main()
