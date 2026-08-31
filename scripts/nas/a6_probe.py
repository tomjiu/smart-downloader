#!/usr/bin/env python3
"""A6 探针: P1(magnet 建任务) -> P4(pause/resume PATCH) -> P5(try_speed 终验) -> P6(ERROR 清理).

依据: A6_PREP_STATIC_CALIBRATION.md 静态校准 + A2-A5 实测定案.
沙盒约束: 引擎拉起->探测->收割必须单脚本进程内完成 (后台 20-130s 被 SIGKILL).
用法: a6_probe.py [p6|p1|full]   (默认 full: P6->P1->轮询->P4->P5->清理)
前置: python3 scripts/nas/a6_ops.py extract && a6_ops.py envconfig  (引擎落位)
"""
import fcntl, json, os, pty, re, select, signal, socket, struct, sys, termios, time
import urllib.request, urllib.error

WS = os.path.expanduser("~/.nas-engine-test")
ENGINE_DIR = os.environ.get("A6_ENGINE_DIR", f"{WS}/engine/bin/bin")
LAUNCHER = f"{ENGINE_DIR}/xunlei-pan-cli-launcher.amd64"
OUT = os.environ.get("A6_OUT", f"{WS}/a6_probe_result.json")
DRIVE = "127.0.0.1:5050"
ROWS, COLS = 40, 120
MAGNET = os.environ.get(
    "A6_MAGNET",
    "magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c"
    "&dn=big-buck-bunny-trailer",  # Big Buck Bunny 公链,  trackerless DHT 可下
)
DELETE_TIMEOUT = 110   # A4/A5 实锤 DELETE 同步阻塞 >75s, 短超时必失败
POLL_MAX = 90          # PENDING->RUNNING/ERROR 观测窗


def drive_up():
    try:
        with socket.create_connection(("127.0.0.1", 5050), timeout=1):
            return True
    except OSError:
        return False


def http(method, path, headers=None, body=None, timeout=12):
    url = f"http://{DRIVE}{path}"
    data = json.dumps(body).encode() if isinstance(body, (dict, list)) else body
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
        "DownloadPipeLimit": "10", "UploadPipeLimit": "10",  # A4: dump 换算 256
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


def jwt_from_homepage():
    st, html = http("GET", "/")
    m = re.search(rb'uiauth\(value\)\{\s*return\s*"([^"]+)"', html)
    return (m.group(1).decode(), st) if m else ("", st)


def log(msg):
    print(msg, flush=True)


def phase_of(task):
    return (task.get("phase") or
            json.loads(task.get("params", "{}")).get("status", "{}") if isinstance(task.get("params"), str) else "")


def cloud_identity(task):
    """从任务对象提取云端身份档 (A6_PREP §8: 第一观测点)."""
    p = task.get("params", {}) if isinstance(task.get("params"), dict) else {}
    return {k: p.get(k) for k in ("client_id", "package_name", "platform",
                                   "device_model", "client_version") if p.get(k)}


def get_task(H, tid):
    import urllib.parse
    filters = urllib.parse.quote(json.dumps({"id": {"in": tid}}))
    st, bd = http("GET", f"/drive/v1/tasks?space=&filters={filters}", headers=H, timeout=15)
    try:
        tasks = json.loads(bd).get("tasks", [])
        return tasks[0] if tasks else None
    except Exception:
        return None


def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "full"
    rep = {"mode": mode, "started_at": int(time.time()), "magnet": MAGNET}

    pid = boot()
    if not pid:
        rep["boot"] = "FAIL"
        json.dump(rep, open(OUT, "w"), ensure_ascii=False, indent=2)
        log("[!] engine boot FAIL")
        return
    log("[+] engine up")

    jwt, st = jwt_from_homepage()
    rep["jwt_status"] = st
    if not jwt:
        rep["jwt"] = "FAIL"
        json.dump(rep, open(OUT, "w"), ensure_ascii=False, indent=2)
        os.kill(pid, signal.SIGTERM)
        return
    log(f"[+] jwt={jwt[:24]}...")
    H = {"pan-auth": jwt, "Device-Space": "", "Content-Type": "application/json"}

    st, bd = http("GET", "/drive/v1/tasks?page_token=&filters=", headers=H)
    rep["tasks_listed"] = {"status": st}
    log(f"[*] list tasks -> {st}")
    try:
        _t0 = json.loads(bd).get("tasks", [])
        if _t0:
            rep["cloud_identity_listing"] = cloud_identity(_t0[0])
            log(f"[*] cloud identity (existing task): {rep['cloud_identity_listing']}")
    except Exception:
        pass

    # ---- P6: 清理历史 ERROR 任务 (DELETE 100s+ 超时) ----
    if mode in ("p6", "full"):
        st, bd = http("GET", "/drive/v1/tasks?page_token=&filters=", headers=H)
        try:
            all_tasks = json.loads(bd).get("tasks", [])
        except Exception:
            all_tasks = []
        err_ids = [t["id"] for t in all_tasks if t.get("phase") == "PHASE_TYPE_ERROR"]
        rep["p6"] = {"error_task_ids": err_ids}
        for tid in err_ids:
            log(f"[*] P6 DELETE {tid} (timeout {DELETE_TIMEOUT}s)...")
            t0 = time.time()
            st, bd = http("DELETE", f"/drive/v1/tasks?space=&task_ids={tid}",
                          headers=H, timeout=DELETE_TIMEOUT)
            rep["p6"][f"del_{tid}"] = {"status": st, "secs": round(time.time() - t0, 1),
                                       "body": bd[:200].decode("utf-8", "replace")}
            log(f"[*] P6 DELETE -> {st} in {rep['p6'][f'del_{tid}']['secs']}s")

    if mode == "p6":
        rep["finished_at"] = int(time.time())
        json.dump(rep, open(OUT, "w"), ensure_ascii=False, indent=2)
        os.kill(pid, signal.SIGTERM)
        log(f"[+] report -> {OUT}")
        return

    # ---- P1: magnet URL 型建任务 (A6_PREP §2 定案载荷) ----
    name = "a6-magnet-bbb"
    payload = {
        "space": "",
        "type": "user#download-url",
        "file_size": "0",
        "name": name,
        "file_name": name,
        "url": {"url": MAGNET},
        "parent_folder_id": "",
        "params": {"target": ""},
    }
    st, bd = http("POST", "/drive/v1/task", headers=H, body=payload)
    rep["p1"] = {"status": st, "body": bd[:800].decode("utf-8", "replace")}
    log(f"[*] P1 POST magnet task -> {st} {bd[:200]!r}")
    try:
        task = json.loads(bd)
        tid = task.get("id", "")
        rep["p1"]["task_id"] = tid
        rep["p1"]["cloud_identity"] = cloud_identity(task)  # §8 第一观测点
        log(f"[*] cloud identity (created task): {rep['p1']['cloud_identity']}")
    except Exception:
        tid = ""
    if not tid:
        rep["finished_at"] = int(time.time())
        json.dump(rep, open(OUT, "w"), ensure_ascii=False, indent=2)
        os.kill(pid, signal.SIGTERM)
        return

    # ---- 轮询相位迁移 ----
    seen = []
    final_phase = ""
    t0 = time.time()
    while time.time() - t0 < POLL_MAX:
        t = get_task(H, tid)
        if t:
            ph = t.get("phase", "")
            if not seen or seen[-1] != (ph, time.time()):
                seen.append({"phase": ph, "t": round(time.time() - t0, 1),
                             "params": str(t.get("params"))[:300]})
                log(f"[*] poll {seen[-1]['t']}s phase={ph} params={str(t.get('params'))[:120]}")
            final_phase = ph
            if ph in ("PHASE_TYPE_RUNNING", "PHASE_TYPE_ERROR",
                      "PHASE_TYPE_COMPLETE", "PHASE_TYPE_PAUSED"):
                break
        time.sleep(4)
    rep["poll"] = seen

    # ---- P4: PATCH pause/resume (A6_PREP §4) ----
    if final_phase == "PHASE_TYPE_RUNNING" and mode == "full":
        for act, expect in (("pause", "PHASE_TYPE_PAUSED"), ("running", "PHASE_TYPE_RUNNING")):
            body = {"space": "", "type": "user#download-url", "id": tid,
                    "set_params": {"spec": json.dumps({"phase": act})}}
            st, bd = http("PATCH", "/drive/v1/task", headers=H, body=body)
            rep.setdefault("p4", {})[act] = {"status": st,
                                             "body": bd[:300].decode("utf-8", "replace")}
            log(f"[*] P4 PATCH {act} -> {st}")
            time.sleep(6)
            t2 = get_task(H, tid)
            got = t2.get("phase", "?") if t2 else "?"
            rep["p4"][act]["phase_after"] = got
            log(f"[*] P4 phase_after={got} (expect {expect})")
            if act == "pause" and got != "PHASE_TYPE_PAUSED":
                break  # 暂停失败则不再 resume
            if act == "running":
                time.sleep(4)

    # ---- P5: try_speed/apply 终验 (仅 RUNNING 生效) ----
    if mode == "full":
        t = get_task(H, tid)
        running = bool(t) and t.get("phase") == "PHASE_TYPE_RUNNING"
        st0, bd0 = http("GET", "/device/v1/try_speed/get_info", headers=H)
        rep["p5_get_info"] = {"status": st0, "body": bd0[:400].decode("utf-8", "replace")}
        if running:
            st, bd = http("POST", "/device/v1/try_speed/apply", headers=H, body={})
            rep["p5_apply"] = {"status": st, "body": bd[:500].decode("utf-8", "replace")}
            log(f"[*] P5 apply -> {st} {bd[:200]!r}")
            st1, bd1 = http("GET", "/device/v1/try_speed/get_info", headers=H)
            rep["p5_after"] = {"status": st1, "body": bd1[:400].decode("utf-8", "replace")}
        else:
            rep["p5_apply"] = {"skipped": f"final_phase={final_phase}"}
            log(f"[!] P5 skipped (phase={final_phase})")

    # ---- 收割: 删除本任务 + 停引擎 ----
    t0 = time.time()
    st, bd = http("DELETE", f"/drive/v1/tasks?space=&task_ids={tid}",
                  headers=H, timeout=DELETE_TIMEOUT)
    rep["reap"] = {"status": st, "secs": round(time.time() - t0, 1)}
    log(f"[*] reap DELETE -> {st}")

    rep["finished_at"] = int(time.time())
    json.dump(rep, open(OUT, "w"), ensure_ascii=False, indent=2)
    try:
        os.kill(pid, signal.SIGTERM)
    except Exception:
        pass
    log(f"[+] report -> {OUT}")


if __name__ == "__main__":
    main()
