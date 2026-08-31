#!/usr/bin/env python3
"""A5: 额度探针 + PipeLimit 复验 + 群晖平台环境指纹伪装实测 + 任务清理.

用法: python3 a5_boot.py <phase>
  phase = baseline | syno | syno-custom | cleanup
关键情报(A5 静态逆向定案):
  - 引擎/launcher 二进制对 synoinfo/authenticate//var/packages 全零命中(含 XOR 全谱)
    => 平台检测为纯 env 指纹, 引擎侧唯一 syno 相关串 = OS_VERSION
  - A4 panic 根因: 只设 PLATFORM=群晖 缺 OS_VERSION => 候选零值
  - cnk3x 权威配方: OS_VERSION="geminilake dsm 7.2-64570" + SYNOPLATFORM + SYNOPKG_*
  - launcher 另含 ALLOW_CUSTOM_PLATFORM env 旋钮(白名单旁路)
"""
import fcntl, json, os, pty, re, select, signal, socket, struct, sys, termios, time
import urllib.request, urllib.error, urllib.parse

ENGINE_DIR = ("/home/z/my-project/repo-smart-downloader/scripts/research/xunlei/"
              "extracted/cross-platform/spk-x64/payload/bin/bin")
LAUNCHER = f"{ENGINE_DIR}/xunlei-pan-cli-launcher.amd64"
WS = os.path.expanduser("~/.nas-engine-test")
DRIVE = "127.0.0.1:5050"
ROWS, COLS = 40, 120
TARGET = "device_id#c7d089aad73f7e2ddd2c263c2956b5a6"
TEST_URL = "https://proof.ovh.net/files/10Mb.dat"

PHASE = sys.argv[1] if len(sys.argv) > 1 else "baseline"
INNER_TAG = sys.argv[2] if len(sys.argv) > 2 else ""
OUT = f"{WS}/a5_{PHASE}.json"
LOG = f"{WS}/logs/engine_a5_{PHASE}.log"

SYNO_ENV = {
    "PLATFORM": "群晖",
    "OS_VERSION": "geminilake dsm 7.2-64570",
    "SYNOPLATFORM": "geminilake",
    "SYNOPKG_PKGNAME": "pan-xunlei-com",
    "SYNOPKG_DSM_VERSION_MAJOR": "7",
    "SYNOPKG_DSM_VERSION_MINOR": "2",
    "SYNOPKG_DSM_VERSION_BUILD": "64570",
}
# 官方 service-setup 实测: 引擎 env 只有 PLATFORM + OS_VERSION (+路径/DriveListen)
SYNO_ENV_OFFICIAL = {
    "PLATFORM": "群晖",
    "OS_VERSION": "geminilake dsm 7.2-64570",
}
SYNO_INFO_CONF = (
    'platform_name="geminilake"\n'
    'synobios="geminilake"\n'
    'unique="synology_geminilake_DS920+"\n'
    'company_title_name="Synology\xe4\xbc\x81\xe9\xb9\x85"\n'
    'default_gateway_enabled="yes"\n'
    'upnpd_enabled="no"\n'
    'timezone="UTC"\n'
)
ETC_VERSION_CONF = (
    '{"productversion":"7.2-64570","buildphase":"GM","buildnumber":"64570",'
    '"smallfixnumber":"0","unique":"synology_geminilake_DS920+",'
    '"platform_name":"geminilake","synobios":"geminilake"}\n'
)
FAKE_ETC = "/tmp/a5_fakeetc"
FAKE_USR = "/tmp/a5_fakeusr"
FAKE_VAR = "/tmp/a5_fakevar"
AUTH_CGI_SRC = ("/home/z/my-project/research/cnk3x-xunlei/embed/authenticate_cgi/"
                "authenticate_cgi_linux_amd64")
PKGDEST_REAL = "/var/packages/pan-xunlei-com/target"
# cnk3x chroot 极简根没有发行版指纹文件 —— 剥离它们, 仿 chroot 最小环境
STRIP_DISTRO_ETC = ["os-release", "lsb-release", "debian_version", "issue", "issue.net",
                    "motd", "hostname", "machine-id", "apt", "dpkg", "alternatives"]


def write_envconfigs():
    yaml_body = "# a5 syno spoof\n" + "".join(
        f'{k}: "{v}"\n' for k, v in {**SYNO_ENV, "SYNOPKG_PKGDEST": PKGDEST_REAL}.items())
    for d in (ENGINE_DIR, f"{WS}/data/.drive/bin"):
        try:
            with open(f"{d}/envconfig", "w") as f:
                f.write(yaml_body)
        except OSError as e:
            print(f"[!] envconfig write fail {d}: {e}", flush=True)


def prepare_fake_trees():
    """三树 shadow 准备: /etc(全拷+syno 件) + /usr(软链农场+authenticate.cgi) + /var(软链农场+packages)"""
    import shutil, subprocess as sp
    sp.run(["rm", "-rf", FAKE_ETC, FAKE_USR, FAKE_VAR], check=False)

    def cp_tree(src, dst, depth=0):
        os.makedirs(dst, exist_ok=True)
        try:
            entries = os.scandir(src)
        except OSError:
            return
        for e in entries:
            s, d = e.path, os.path.join(dst, e.name)
            try:
                if e.is_symlink():
                    os.symlink(os.readlink(s), d)
                elif e.is_dir(follow_symlinks=False) and depth < 4:
                    cp_tree(s, d, depth + 1)
                elif e.is_file(follow_symlinks=False) and e.stat().st_size < 8 * 1024 * 1024:
                    shutil.copy(s, d)
            except OSError:
                pass

    # /etc
    cp_tree("/etc", FAKE_ETC)
    with open(f"{FAKE_ETC}/synoinfo.conf", "w") as f:
        f.write(SYNO_INFO_CONF)
    with open(f"{FAKE_ETC}/VERSION", "w") as f:
        f.write(ETC_VERSION_CONF)
    if PHASE in ("syno-min",):
        for name in STRIP_DISTRO_ETC:
            p = f"{FAKE_ETC}/{name}"
            try:
                if os.path.isdir(p) and not os.path.islink(p):
                    import shutil
                    shutil.rmtree(p)
                else:
                    os.remove(p)
            except OSError:
                pass
    # /usr: 软链农场(回指 /tmp/usr-real) + syno 树
    os.makedirs(f"{FAKE_USR}/syno/synoman/webman/modules", exist_ok=True)
    shutil.copy(AUTH_CGI_SRC, f"{FAKE_USR}/syno/synoman/webman/modules/authenticate.cgi")
    os.chmod(f"{FAKE_USR}/syno/synoman/webman/modules/authenticate.cgi", 0o777)
    for name in ("lib", "lib64", "bin", "sbin", "libexec", "share", "local", "src"):
        if os.path.exists(f"/usr/{name}"):
            os.symlink(f"/tmp/usr-real/{name}", f"{FAKE_USR}/{name}")
    # /var: 软链农场 + packages 真实布局
    os.makedirs(f"{FAKE_VAR}/packages/pan-xunlei-com/target/var", exist_ok=True)
    os.makedirs(f"{FAKE_VAR}/packages/pan-xunlei-com/target/bin", exist_ok=True)
    os.symlink(ENGINE_DIR, f"{FAKE_VAR}/packages/pan-xunlei-com/target/bin/bin")
    for e in os.scandir("/var"):
        if e.name == "packages":
            continue
        try:
            if e.is_symlink():
                os.symlink(os.readlink(e.path), f"{FAKE_VAR}/{e.name}")
            else:
                os.symlink(f"/tmp/var-real/{e.name}", f"{FAKE_VAR}/{e.name}")
        except OSError:
            pass
    return {"etc": FAKE_ETC, "usr": FAKE_USR, "var": FAKE_VAR}


def prepare_syno_tree():
    """cnk3x 布局: $PKGDEST/bin/bin -> ENGINE_DIR(含 version), $PKGDEST/var"""
    pkgdest = f"{WS}/syno/var/packages/pan-xunlei-com/target"
    os.makedirs(f"{pkgdest}/var", exist_ok=True)
    os.makedirs(f"{pkgdest}/bin", exist_ok=True)
    ln = f"{pkgdest}/bin/bin"
    if not os.path.exists(ln) and not os.path.islink(ln):
        os.symlink(ENGINE_DIR, ln)
    return pkgdest


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


def boot(extra_env=None):
    env = {k: v for k, v in os.environ.items() if k != "PLATFORM"}
    env.update({
        "DriveListen": DRIVE, "LauncherListen": "127.0.0.1:5051",
        "ConfigPath": f"{WS}/data", "DownloadPATH": f"{WS}/downloads",
        "HOME": f"{WS}/data/.drive", "GIN_MODE": "release",
        "TERM": "xterm", "COLUMNS": str(COLS), "LINES": str(ROWS),
    })
    if extra_env:
        env.update(extra_env)
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(ENGINE_DIR)
        os.execve(LAUNCHER, [LAUNCHER, "-pid", f"{WS}/engine.pid"], env)
        os._exit(127)
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    t0 = time.time()
    while time.time() - t0 < 55:
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
    return pid  # 超时也返回, 让上层按日志判 panic


def log_text():
    try:
        return open(LOG, errors="replace").read()
    except OSError:
        return ""


def log_sig(log):
    sig = {}
    m = re.search(r"config\.init init succ: (&\{.{0,2600})", log, re.S)
    if m:
        sig["config_init"] = m.group(1)[:2600]
    sig["panic"] = "platform not suport" in log
    sig["platform_field"] = (re.search(r" Platform:(\S+?) ", log) or [None, None])[1]
    priv = re.findall(r"[Pp]latform[Pp]rivilege[^,\n]{0,80}", log)
    sig["privilege_lines"] = priv[:5]
    pk = re.findall(r'"package_name":"([^"]+)"', log)
    sig["package_name"] = pk[:2]
    return sig


def main():
    global OUT, LOG
    if PHASE == "__inner__" and INNER_TAG:
        OUT = f"{WS}/a5_{INNER_TAG}.json"
        LOG = f"{WS}/logs/engine_a5_{INNER_TAG}.log"
    R = {"phase": PHASE, "started_at": int(time.time())}
    os.makedirs(f"{WS}/logs", exist_ok=True)
    open(LOG, "w").close()

    extra = {}
    if PHASE == "baseline":
        extra = {"DownloadPipeLimit": "10", "UploadPipeLimit": "10"}
    elif PHASE == "syno":
        pkgdest = prepare_syno_tree()
        extra = dict(SYNO_ENV)
        extra["SYNOPKG_PKGDEST"] = pkgdest
    elif PHASE == "syno-custom":
        trees = prepare_fake_trees()
        os.makedirs("/tmp/usr-real", exist_ok=True)
        os.makedirs("/tmp/var-real", exist_ok=True)
        write_envconfigs()
        with open(f"{ENGINE_DIR}/envconfig", "a") as f:
            f.write('ALLOW_CUSTOM_PLATFORM: "true"\n')
        try:
            with open(f"{WS}/data/.drive/bin/envconfig", "a") as f:
                f.write('ALLOW_CUSTOM_PLATFORM: "true"\n')
        except OSError:
            pass
        print("[*] syno-custom (ALLOW_CUSTOM_PLATFORM) ready, re-exec inside unshare -Urm ...", flush=True)
        try:
            os.execvp("unshare", ["unshare", "-Urm", sys.executable, os.path.abspath(__file__), "__inner__", "syno-custom"])
        finally:
            try:
                os.remove(f"{ENGINE_DIR}/envconfig")
            except OSError:
                pass
    elif PHASE == "syno-final":
        trees = prepare_fake_trees()
        os.makedirs("/tmp/usr-real", exist_ok=True)
        os.makedirs("/tmp/var-real", exist_ok=True)
        write_envconfigs()
        with open(f"{ENGINE_DIR}/envconfig", "a") as f:
            f.write('ALLOW_CUSTOM_PLATFORM: "true"\n')
        try:
            with open(f"{WS}/data/.drive/bin/envconfig", "a") as f:
                f.write('ALLOW_CUSTOM_PLATFORM: "true"\n')
        except OSError:
            pass
        print("[*] syno-final (custom+PipeLimit) ready, re-exec inside unshare -Urm ...", flush=True)
        try:
            os.execvp("unshare", ["unshare", "-Urm", sys.executable, os.path.abspath(__file__), "__inner__", "syno-final"])
        finally:
            try:
                os.remove(f"{ENGINE_DIR}/envconfig")
            except OSError:
                pass
    elif PHASE == "syno-min":
        trees = prepare_fake_trees()
        os.makedirs("/tmp/usr-real", exist_ok=True)
        os.makedirs("/tmp/var-real", exist_ok=True)
        write_envconfigs()
        print("[*] syno-min (stripped etc) ready, re-exec inside unshare -Urm ...", flush=True)
        try:
            os.execvp("unshare", ["unshare", "-Urm", sys.executable, os.path.abspath(__file__), "__inner__", "syno-min"])
        finally:
            try:
                os.remove(f"{ENGINE_DIR}/envconfig")
            except OSError:
                pass
    elif PHASE == "syno-full":
        trees = prepare_fake_trees()
        os.makedirs("/tmp/usr-real", exist_ok=True)
        os.makedirs("/tmp/var-real", exist_ok=True)
        print(f"[*] fake trees ready {trees}, re-exec inside unshare -Urm ...", flush=True)
        os.execvp("unshare", ["unshare", "-Urm", sys.executable, os.path.abspath(__file__), "__inner__", "syno-full"])
    elif PHASE == "syno-envcfg":
        trees = prepare_fake_trees()
        os.makedirs("/tmp/usr-real", exist_ok=True)
        os.makedirs("/tmp/var-real", exist_ok=True)
        write_envconfigs()
        print(f"[*] fake trees + envconfig ready, re-exec inside unshare -Urm ...", flush=True)
        try:
            os.execvp("unshare", ["unshare", "-Urm", sys.executable, os.path.abspath(__file__), "__inner__", "syno-envcfg"])
        finally:
            try:
                os.remove(f"{ENGINE_DIR}/envconfig")
            except OSError:
                pass
    elif PHASE == "__inner__":
        import subprocess
        R["inner_binds"] = {}
        seq = [
            ("usr-keep", ["mount", "--bind", "/usr", "/tmp/usr-real"]),
            ("var-keep", ["mount", "--bind", "/var", "/tmp/var-real"]),
            ("usr-shadow", ["mount", "--bind", FAKE_USR, "/usr"]),
            ("var-shadow", ["mount", "--bind", FAKE_VAR, "/var"]),
            ("etc-shadow", ["mount", "--bind", FAKE_ETC, "/etc"]),
        ]
        for name, cmd in seq:
            r = subprocess.run(cmd, capture_output=True, text=True)
            R["inner_binds"][name] = r.returncode
            print(f"[*] bind {name} rc={r.returncode} {r.stderr[:100]}", flush=True)
        checks = {
            "synoinfo": os.path.exists("/etc/synoinfo.conf"),
            "authcgi": os.path.exists("/usr/syno/synoman/webman/modules/authenticate.cgi"),
            "pkgdest_version": os.path.exists(f"{PKGDEST_REAL}/bin/bin/version"),
            "loader": os.path.exists("/usr/lib64/ld-linux-x86-64.so.2"),
            "orig_usr": os.path.exists("/tmp/usr-real/lib64/ld-linux-x86-64.so.2"),
            "orig_var": os.path.exists("/tmp/var-real"),
        }
        R["inner_checks"] = checks
        print(f"[*] checks {checks}", flush=True)
        if not all(checks.values()):
            json.dump(R, open(OUT, "w"), ensure_ascii=False, indent=2)
            return
        extra = dict(SYNO_ENV)
        extra["SYNOPKG_PKGDEST"] = PKGDEST_REAL
        _tag = INNER_TAG or ""
        if _tag in ("syno-custom", "syno-final"):
            extra["ALLOW_CUSTOM_PLATFORM"] = "true"
        if _tag == "syno-final":
            extra["DownloadPipeLimit"] = "10"
            extra["UploadPipeLimit"] = "10"
    elif PHASE == "cleanup":
        extra = {"DownloadPipeLimit": "10", "UploadPipeLimit": "10"}

    pid = boot(extra)
    R["boot"] = bool(pid)
    print(f"[+] {PHASE} boot={bool(pid)}", flush=True)
    log = log_text()
    R["log_sig"] = log_sig(log)
    print(f"[*] panic={R['log_sig']['panic']} platform={R['log_sig']['platform_field']}", flush=True)
    if R["log_sig"]["panic"] or not pid:
        R["log_head"] = log[:2500]
        json.dump(R, open(OUT, "w"), ensure_ascii=False, indent=2)
        if pid:
            os.kill(pid, signal.SIGTERM)
        print(f"[!] panic/failed -> {OUT}")
        return

    st, html = http("GET", "/")
    m = re.search(rb'uiauth\(value\)\{ return "([^"]+)"', html)
    R["jwt_ok"] = bool(m)
    if not m:
        R["html_head"] = html[:300].decode("utf-8", "replace")
        json.dump(R, open(OUT, "w"), ensure_ascii=False, indent=2)
        os.kill(pid, signal.SIGTERM)
        return
    jwt = m.group(1).decode()
    H = {"pan-auth": jwt, "Content-Type": "application/json"}
    q = urllib.parse.quote

    st, bd = http("GET", f"/drive/v1/tasks?space={q(TARGET)}&filters={q(json.dumps({'type': {'in': 'user#download-url,user#download'}}))}", headers=H)
    R["list"] = {"status": st, "n": 0, "tasks": []}
    if st == 200:
        tasks = json.loads(bd).get("tasks", [])
        R["list"]["n"] = len(tasks)
        R["list"]["tasks"] = [{"id": t.get("id"), "name": t.get("name"), "phase": t.get("phase"),
                               "message": t.get("message"),
                               "error": t.get("params", {}).get("error")} for t in tasks]
    print(f"[*] list -> {st} n={R['list']['n']}", flush=True)

    _TAG = INNER_TAG or PHASE
    if _TAG in ("baseline", "syno", "syno-custom", "syno-final"):
        name = f"a5-{_TAG}-{int(time.time())}.dat"
        payload = {
            "space": TARGET, "type": "user#download-url", "file_size": "0",
            "name": name, "file_name": name,
            "url": {"url": TEST_URL}, "parent_folder_id": "",
            "params": {"target": TARGET},
        }
        st2, bd2 = http("POST", "/drive/v1/task", headers=H, body=payload)
        R["create"] = {"status": st2, "body": bd2[:500].decode("utf-8", "replace")}
        print(f"[*] create -> {st2} {bd2[:200]!r}", flush=True)
        if st2 == 200:
            tid = json.loads(bd2).get("task", {}).get("id") or json.loads(bd2).get("id")
            R["create"]["task_id"] = tid
            seen = []
            applied = False
            t0 = time.time()
            while time.time() - t0 < 35:
                st3, bd3 = http("GET", f"/drive/v1/tasks?space={q(TARGET)}&filters={q(json.dumps({'id': {'in': tid}}))}", headers=H, timeout=5)
                if st3 == 200:
                    t = json.loads(bd3)["tasks"][0]
                    s = f"{t.get('phase')}|{t.get('progress')}|{t.get('message')}|{t.get('params',{}).get('speed')}|{t.get('params',{}).get('error')}"
                    if not seen or seen[-1].split(" ", 1)[-1] != s:
                        line = f"t={time.time()-t0:3.0f}s {s}"
                        seen.append(line)
                        print("    " + line, flush=True)
                    ph = str(t.get("phase", ""))
                    if "RUNNING" in ph and not applied:
                        st4, bd4 = http("POST", "/device/v1/try_speed/apply", headers=H, body={}, timeout=8)
                        R["apply"] = {"status": st4, "body": bd4[:300].decode("utf-8", "replace"), "at": s}
                        print(f"    >>> APPLY -> {st4} {bd4[:200]!r}", flush=True)
                        applied = True
                    if "COMPLETE" in ph or "ERROR" in ph or "FAILED" in ph:
                        break
                time.sleep(1.5)
            R["poll"] = seen
            R["applied"] = applied

    st5, bd5 = http("GET", "/device/v1/try_speed/get_info", headers=H)
    R["get_info"] = {"status": st5, "body": bd5[:300].decode("utf-8", "replace")}

    if PHASE == "cleanup":
        ids = [t["id"] for t in R["list"]["tasks"] if t.get("id")]
        if ids:
            qq = "&".join(f"task_ids={i}" for i in ids)
            st6, bd6 = http("DELETE", f"/drive/v1/tasks?space={q(TARGET)}&{qq}", headers=H, timeout=75)
            R["delete"] = {"status": st6, "body": bd6[:400].decode("utf-8", "replace"), "ids": ids}
            print(f"[*] DELETE x{len(ids)} -> {st6} {bd6[:200]!r}", flush=True)
            st7, bd7 = http("GET", f"/drive/v1/tasks?space={q(TARGET)}&filters={q(json.dumps({'type': {'in': 'user#download-url,user#download'}}))}", headers=H)
            R["after_delete"] = {"status": st7, "n": len(json.loads(bd7).get("tasks", [])) if st7 == 200 else bd7[:150].decode("utf-8", "replace")}
            print(f"[*] verify -> {st7} n={R['after_delete']['n']}", flush=True)

    R["log_tail"] = log[-2200:]
    R["finished_at"] = int(time.time())
    json.dump(R, open(OUT, "w"), ensure_ascii=False, indent=2)
    if pid:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    print(f"[+] report -> {OUT}")


if __name__ == "__main__":
    main()
