#!/usr/bin/env python3
"""A2 一键校准器（假设区 #8/#9/#10）—— 扫码 token 到手后运行。

流程（对应附录 E.5 假设区）：
  1) token    — token 文件形状记录 + 远端有效性验证（#8 预置格式校准）
  2) engine   — 预置 token 拉起 xllite 引擎，验证「免扫码登录门」是否通过
  3) api      — DriveListen(127.0.0.1:5050) gin 路由面探测（#9 API 形状）
  4) tryspeed — /device/v1/try_speed/* 参数面探测（#10 试用加速，仅探测不消费）

用法：
  python3 a2_calibrate.py                          # 全步骤（token,engine,api,tryspeed）
  python3 a2_calibrate.py --steps token,api        # 引擎已由 nas_engine_run.sh 拉起时
  python3 a2_calibrate.py --engine /path/xunlei-pan-cli.3.23.5.amd64 --workspace /tmp/nasws

所有路径/端口默认值均可被环境变量覆盖（SD_A2_TOKEN_FILE / SD_A2_WORKSPACE /
SD_A2_ENGINE / SD_A2_DRIVE_LISTEN / SD_XL_CLIENT_ID），引擎候选路径相对脚本自身定位。
输出：<workspace>/a2_result.json（字段形状 + 状态码，secret 一律脱敏）+ 控制台校准结论。
零第三方依赖（Python 3 stdlib only）；引擎不存在时自动降级为只跑远端探测。
"""
import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
# 常量区：引擎内嵌 OAuth 客户端（附录 E.2.3；env 可覆盖以便轮换）
CLIENT_ID = os.environ.get("SD_XL_CLIENT_ID", "X9ibISwpIp8jQ4Ya")
DEFAULT_TOKEN_FILE = os.environ.get(
    "SD_A2_TOKEN_FILE",
    os.path.expanduser("~/.nas-engine-test/data/.drive/auth_token.json"))
DEFAULT_WORKSPACE = os.environ.get(
    "SD_A2_WORKSPACE", os.path.expanduser("~/.nas-engine-test"))
# 引擎候选：优先脚本同仓的 research 提取物（任意克隆位置自适应），再退 CWD
DEFAULT_ENGINE_CANDIDATES = [
    os.environ.get("SD_A2_ENGINE", ""),
    os.path.normpath(os.path.join(
        SCRIPT_DIR, "..", "research", "xunlei", "extracted", "cross-platform",
        "spk-x64", "payload", "bin", "bin", "xunlei-pan-cli.3.23.5.amd64")),
    "./xunlei-pan-cli.3.23.5.amd64",
]
DRIVE_LISTEN = os.environ.get("SD_A2_DRIVE_LISTEN", "127.0.0.1:5050")

# 假设区 #9 候选路由（gin 前缀 /device/v1/* 与 drive v1 混编，实测定形）
API_PROBES = [
    ("GET", "/", None),
    ("GET", "/webman/3rdparty/pan-xunlei-com/index.cgi/", None),  # 群晖兼容面
    ("GET", "/drive/v1/user/info", None),
    ("GET", "/drive/v1/tasks", None),
    ("GET", "/drive/v1/events", None),
    ("GET", "/device/v1/info", None),
    ("GET", "/device/v1/config", None),
    ("POST", "/device/v1/try_speed/get_info", {}),
]
TRYSPEED_PROBES = [  # 假设区 #10：参数面形状（不触发真实加速消耗）
    ("POST", "/device/v1/try_speed/get_info", {}),
    ("POST", "/device/v1/try_speed/get_info", {"file_size": 104857600}),
    ("GET", "/device/v1/try_speed/get_info", None),
    ("POST", "/device/v1/try_speed/apply", {}),
]


def shape(v, depth=0):
    """JSON 形状描述：字段名 + 类型；字符串值脱敏（>12 字符只留前 8 位）。"""
    if depth > 4:
        return "…"
    if isinstance(v, dict):
        return {k: shape(x, depth + 1) for k, x in v.items()}
    if isinstance(v, list):
        return [shape(v[0], depth + 1), f"…({len(v)} items)"] if v else []
    if isinstance(v, str) and len(v) > 12:
        return v[:8] + "…<redacted>"
    return v


def http_req(method, url, headers=None, body=None, timeout=6):
    """返回 (status, shape_or_text)；4xx/5xx 也返回（形状探测要看错误体）。"""
    data = json.dumps(body).encode() if isinstance(body, dict) else (body.encode() if isinstance(body, str) else None)
    req = urllib.request.Request(url, data=data, method=method, headers=headers or {})
    if isinstance(body, dict):
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read(4096).decode("utf-8", "replace")
            return r.status, _body_shape(raw)
    except urllib.error.HTTPError as e:
        raw = e.read(4096).decode("utf-8", "replace")
        return e.code, _body_shape(raw)
    except Exception as e:
        return None, f"<conn-error {type(e).__name__}: {e}>"


def _body_shape(raw):
    try:
        return shape(json.loads(raw))
    except Exception:
        return raw[:160]


def auth_headers(tok):
    return {"Authorization": f"Bearer {tok['access_token']}", "x-client-id": CLIENT_ID}


# ---------------------------------------------------------------- steps

def step_token(tok, report):
    print("\n== 步骤 1：token 形状与有效性（假设区 #8）==")
    report["token_file_shape"] = shape(tok)
    print("[*] 文件字段形状:", json.dumps(report["token_file_shape"], ensure_ascii=False))
    probes = [
        ("api-pan user/info", "GET", "https://api-pan.xunlei.com/drive/v1/user/info"),
        ("xluser user/info", "GET", "https://xluser-ssl.xunlei.com/v1/user/info"),
    ]
    report["token_remote"] = {}
    for name, m, url in probes:
        st, bd = http_req(m, url, headers=auth_headers(tok))
        report["token_remote"][name] = {"status": st, "body": bd}
        print(f"[*] {name} -> {st} {json.dumps(bd, ensure_ascii=False)[:200]}")


def find_engine(arg):
    if arg:
        return arg if os.path.exists(arg) else None
    for c in DEFAULT_ENGINE_CANDIDATES:
        if os.path.exists(c):
            return c
    return None


def step_engine(tok, engine, ws, report, keep):
    print("\n== 步骤 2：预置 token 拉起引擎（免扫码登录门，#8 实测）==")
    data = os.path.join(ws, "data")
    dl = os.path.join(ws, "downloads")
    logs = os.path.join(ws, "logs")
    drive_home = os.path.join(data, ".drive")
    for d in (os.path.join(drive_home), dl, logs):
        os.makedirs(d, exist_ok=True)
    tok_path = os.path.join(drive_home, "auth_token.json")
    with open(tok_path, "w") as f:
        json.dump(tok, f, indent=2)
    os.chmod(tok_path, 0o600)
    print(f"[+] token 预置 -> {tok_path}")

    env = {k: v for k, v in os.environ.items() if k != "PLATFORM"}  # ⚠ 脏 PLATFORM 必须剔除
    env.update({
        "DriveListen": DRIVE_LISTEN,
        "LauncherListen": "127.0.0.1:5051",
        "ConfigPath": data,
        "DownloadPATH": dl,
        "HOME": drive_home,
        "GIN_MODE": "release",
        "DriveAuthorizationTokenPath": tok_path,  # #8 预置口（config.init dump 键名）
    })
    logf = open(os.path.join(logs, "engine.log"), "w")
    proc = subprocess.Popen([engine, "-pid", os.path.join(ws, "engine.pid")],
                            cwd=os.path.dirname(engine), env=env,
                            stdout=logf, stderr=subprocess.STDOUT)
    report["engine"] = {"bin": engine, "pid": proc.pid}
    print(f"[+] 引擎已拉起 pid={proc.pid}，等待 DriveListen {DRIVE_LISTEN} …")
    up = False
    host, port = DRIVE_LISTEN.split(":")
    for i in range(45):
        time.sleep(1)
        if proc.poll() is not None:
            print(f"[!] 引擎提前退出 exit={proc.returncode}（看 {logs}/engine.log）")
            break
        try:
            with socket.create_connection((host, int(port)), timeout=1):
                up = True
                print(f"[+] t={i + 1}s DriveListen TCP 就绪")
                break
        except OSError:
            continue
    log_tail = _read_tail(os.path.join(logs, "engine.log"))
    report["engine"].update({
        "drive_listen_up": up,
        "exited": proc.poll(),
        "log_tail": log_tail,
        "login_gate": _classify_login(log_tail),
    })
    print(f"[*] 登录门判定: {report['engine']['login_gate']}")
    if not keep and proc.poll() is None:
        proc.terminate()
        print("[*] 引擎已停止（--keep-engine 可保留）")
    return proc if (keep and proc.poll() is None) else None


def _read_tail(p, n=60):
    try:
        with open(p, "r", errors="replace") as f:
            return "".join(f.readlines()[-n:])
    except OSError:
        return ""


def _classify_login(log_tail):
    if "panic" in log_tail and "/dev/tty" in log_tail:
        return "PANIC:no-tty（token 未被接受？）"
    if "auth/device/code" in log_tail or "device_code" in log_tail:
        return "FALLBACK:发起设备码扫码（token 未被接受）"
    if "欢迎使用" in log_tail or "xllite" in log_tail:
        return "BOOTED（横幅出现，需结合 api 步骤确认登录态）"
    return "UNKNOWN"


def step_api(report, base=None):
    print("\n== 步骤 3：DriveListen gin 路由面（假设区 #9）==")
    base = base or f"http://{DRIVE_LISTEN}"
    report["api_probe"] = {}
    for m, path, body in API_PROBES:
        st, bd = http_req(m, base + path, body=body)
        verdict = {200: "OK", 401: "存在·需鉴权", 403: "存在·需鉴权",
                   404: "不存在", 405: "存在·方法不对"}[st] if st else "引擎未运行"
        report["api_probe"][f"{m} {path}"] = {"status": st, "verdict": verdict, "body": bd}
        print(f"[*] {m:4} {path:52} -> {st} {verdict} {json.dumps(bd, ensure_ascii=False)[:120]}")


def step_tryspeed(tok, report, base=None):
    print("\n== 步骤 4：try_speed 参数面（假设区 #10）==")
    base = base or f"http://{DRIVE_LISTEN}"
    report["tryspeed"] = {}
    hs = auth_headers(tok)
    for m, path, body in TRYSPEED_PROBES:
        st, bd = http_req(m, base + path, headers=hs, body=body)
        report["tryspeed"][f"{m} {path} {json.dumps(body)}"] = {"status": st, "body": bd}
        print(f"[*] {m:4} {path:36} body={json.dumps(body)} -> {st} {json.dumps(bd, ensure_ascii=False)[:140]}")
    # 远端对照（路由可能只在服务端）
    st, bd = http_req("POST", "https://api-pan.xunlei.com/device/v1/try_speed/get_info",
                      headers=hs, body={})
    report["tryspeed"]["REMOTE api-pan get_info"] = {"status": st, "body": bd}
    print(f"[*] 远端 api-pan.xunlei.com/device/v1/try_speed/get_info -> {st} {json.dumps(bd, ensure_ascii=False)[:140]}")


# ---------------------------------------------------------------- main

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--token-file", default=DEFAULT_TOKEN_FILE)
    ap.add_argument("--engine", default=None, help="xunlei-pan-cli 二进制路径")
    ap.add_argument("--workspace", default=DEFAULT_WORKSPACE)
    ap.add_argument("--steps", default="token,engine,api,tryspeed")
    ap.add_argument("--keep-engine", action="store_true")
    args = ap.parse_args()
    steps = set(args.steps.split(","))

    tok = None
    if os.path.exists(args.token_file):
        with open(args.token_file) as f:
            tok = json.load(f)
        print(f"[+] token 文件: {args.token_file}")
    else:
        print(f"[!] token 文件不存在: {args.token_file}（先跑 nas_qr_daemon.py 扫码）")

    report = {"started_at": int(time.time()), "client_id": CLIENT_ID}
    engine_proc = None
    try:
        if "token" in steps:
            if tok:
                step_token(tok, report)
            else:
                report["token_file_shape"] = "MISSING"
        engine_bin = find_engine(args.engine)
        if "engine" in steps:
            if tok and engine_bin:
                engine_proc = step_engine(tok, engine_bin, args.workspace, report, args.keep_engine)
            else:
                report["engine"] = "SKIPPED（缺 token 或引擎二进制）"
                print("[!] 跳过引擎步骤（缺 token 或引擎二进制）")
        if "api" in steps:
            step_api(report)
        if "tryspeed" in steps:
            if tok:
                step_tryspeed(tok, report)
            else:
                report["tryspeed"] = "SKIPPED（缺 token）"
    finally:
        if engine_proc:
            engine_proc.send_signal(signal.SIGTERM)
        report["finished_at"] = int(time.time())
        out = os.path.join(args.workspace, "a2_result.json")
        os.makedirs(args.workspace, exist_ok=True)
        with open(out, "w") as f:
            json.dump(report, f, ensure_ascii=False, indent=2)
        print(f"\n[+] 报告已写 {out}")
        print("== 校准结论 ==")
        eng = report.get("engine")
        if isinstance(eng, dict):
            print(f"  #8 token 预置: {'通过' if eng.get('login_gate', '').startswith('BOOTED') else eng.get('login_gate')}")
            print(f"  #9 API 形状: {sum(1 for v in report.get('api_probe', {}).values() if v['status'] not in (None, 404))} 个候选路由存活")
        else:
            print("  #8/#9: 引擎步骤未运行（本地跑需 token + 引擎二进制）")


if __name__ == "__main__":
    main()
