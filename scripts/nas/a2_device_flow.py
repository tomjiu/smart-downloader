#!/usr/bin/env python3
"""A2 设备码流解耦执行器——对抗沙盒「调用结束即回收进程」的约束。

原理：RFC 8628 设备码授权的等待状态在**服务端**，本地进程无需常驻：
  request : 申请 device_code → 落盘 device_flow.json → 打印授权 URL → 退出
  poll    : （用户已在浏览器完成授权后）拿 device_code 换 token → 写 preset 文件
  status  : 单次无副作用探测

端点与凭据：NAS 引擎内嵌 client（附录 E.2.3）
  client_id / client_secret 通过环境变量注入（SD_XL_CLIENT_ID / SD_XL_CLIENT_SECRET）
  scope="pan user profile sso offline pan/xunlei/share/create"

device_id 取引擎工作区确定性值（硬件哈希，跨重启不变）：
  c7d089aad73f7e2ddd2c263c2956b5a6（engine_pty.log 实证）
"""
import json
import os
import sys
import time
import urllib.error
import urllib.request

CLIENT_ID = os.environ["SD_XL_CLIENT_ID"]
CLIENT_SECRET = os.environ["SD_XL_CLIENT_SECRET"]
SCOPE = "pan user profile sso offline pan/xunlei/share/create"
CODE_URL = "https://xluser-ssl.xunlei.com/v1/auth/device/code"
TOKEN_URL = "https://xluser-ssl.xunlei.com/v1/auth/token"
GRANT = "urn:ietf:params:oauth:grant-type:device_code"
DEVICE_ID = os.environ.get("SD_DEVICE_ID", "c7d089aad73f7e2ddd2c263c2956b5a6")

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
WS = os.path.expanduser("~/.nas-engine-test")
FLOW = f"{WS}/device_flow.json"
TOKP = f"{WS}/data/.drive/auth_token.json"
ARCHIVE = os.environ.get("SD_QR_ARCHIVE", os.path.normpath(os.path.join(
    SCRIPT_DIR, "..", "research", "xunlei", "extracted", "cross-platform",
    "xllite_token.json")))


def post(url, obj):
    data = json.dumps(obj).encode()
    req = urllib.request.Request(url, data=data, method="POST",
                                 headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return r.status, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read().decode())
        except Exception:
            return e.code, {}


def yc_url(user_code):
    return ("https://pan.xunlei.com/yc/?client_id=" + CLIENT_ID +
            "&justOne=1&noActionBar=true&noStatusBar=true&platform=docker&plm=doc"
            "&privilege=PLATFORM_DOCKER&runner_space=&space=device_id%23" + DEVICE_ID +
            "&user_code=" + user_code)


def cmd_request():
    st, r = post(CODE_URL, {"client_id": CLIENT_ID, "client_secret": CLIENT_SECRET,
                            "scope": SCOPE})
    if st != 200 or "device_code" not in r:
        print("REQUEST FAIL:", st, json.dumps(r, ensure_ascii=False)[:300])
        sys.exit(1)
    flow = {"device_code": r["device_code"], "user_code": r.get("user_code", ""),
            "expires_in": r.get("expires_in"), "interval": r.get("interval", 5),
            "created_at": int(time.time())}
    os.makedirs(WS, exist_ok=True)
    json.dump(flow, open(FLOW, "w"), indent=2)
    print("user_code :", flow["user_code"])
    print("expires_in:", flow["expires_in"], "(设备码有效期)")
    print("YC_URL    :", yc_url(flow["user_code"]))
    if r.get("verification_uri_complete"):
        print("XLUSER_URL:", r["verification_uri_complete"])


def save_token(r):
    """fresh token 落盘：引擎 preset 文件 + 取证归档。"""
    os.makedirs(os.path.dirname(TOKP), exist_ok=True)
    json.dump(r, open(TOKP, "w"), indent=2)
    os.chmod(TOKP, 0o600)
    os.makedirs(os.path.dirname(ARCHIVE), exist_ok=True)
    json.dump({"fetched_at": int(time.time()), "via": "a2_device_flow poll",
               "response": r}, open(ARCHIVE, "w"), indent=2)
    print("[+] preset 文件 ->", TOKP)
    print("[+] 取证归档   ->", ARCHIVE)


def cmd_poll(timeout):
    flow = json.load(open(FLOW))
    age = int(time.time()) - flow["created_at"]
    print(f"device_code 签发于 {age}s 前（expires_in={flow.get('expires_in')}）；轮询至多 {timeout}s")
    t0, n, interval = time.time(), 0, max(3, flow.get("interval", 5))
    while time.time() - t0 < timeout:
        n += 1
        st, r = _poll(flow)
        if st == 200 and r.get("access_token"):
            print(f"POLL OK round={n}")
            shape = {k: (v[:12] + "…" if isinstance(v, str) and len(v) > 16 else v)
                     for k, v in r.items()}
            print("token 字段:", json.dumps(shape, ensure_ascii=False)[:400])
            save_token(r)
            return
        err = r.get("error") or ""
        desc = r.get("error_description") or json.dumps(r, ensure_ascii=False)[:140]
        print(f"round={n} status={st} err={err} {desc}")
        if err in ("expired_token", "invalid_grant", "access_denied"):
            print("CODE DEAD — 请重新 request 换新码")
            sys.exit(3)
        if err == "slow_down":
            interval += 5
        time.sleep(interval)
    print("POLL TIMEOUT — 用户未确认或未完成；可再次 poll 或重新 request")
    sys.exit(4)


def _poll(flow):
    return post(TOKEN_URL, {"grant_type": GRANT, "client_id": CLIENT_ID,
                            "client_secret": CLIENT_SECRET,
                            "device_code": flow["device_code"], "scope": SCOPE})


def cmd_status():
    flow = json.load(open(FLOW))
    st, r = _poll(flow)
    err = r.get("error") or ("OK" if r.get("access_token") else "?")
    print(f"status={st} err={err} age={int(time.time()) - flow['created_at']}s")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "request"
    if cmd == "request":
        cmd_request()
    elif cmd == "poll":
        cmd_poll(int(sys.argv[2]) if len(sys.argv) > 2 else 90)
    elif cmd == "status":
        cmd_status()
    else:
        print(__doc__)
