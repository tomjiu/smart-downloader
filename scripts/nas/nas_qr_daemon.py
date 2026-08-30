#!/usr/bin/env python3
"""xllite 扫码授权守护：循环续发设备码（120s/码），最长运行 30 分钟。
- 每个码发起后立即开始轮询 token（interval=2s）
- 到手 token → 落盘预置路径 + 取证归档 → 退出
- 未授权过期 → 自动发下一个码，状态写 qr_state.json（含最新短链）
用法：nohup python3 nas_qr_daemon.py &
"""
import json, os, sys, time, urllib.request, urllib.parse

CLIENT_ID = "X9ibISwpIp8jQ4Ya"
CLIENT_SECRET = "BlPF2z7HEeutzH4t6zyjLw"
SCOPE = "pan user profile sso offline pan/xunlei/share/create"
CODE_URL = "https://xluser-ssl.xunlei.com/v1/auth/device/code"
TOKEN_URL = "https://xluser-ssl.xunlei.com/v1/auth/token"
STATE = os.path.expanduser("~/my-project/scripts/research/xunlei/qr_state.json")
HOME = os.path.expanduser("~/.nas-engine-test/data/.drive")
ARCH = os.path.expanduser(
    "~/my-project/repo-smart-downloader/scripts/research/xunlei/extracted/cross-platform/xllite_token_20260830.json")
MAX_RUN = 2 * 60 * 60

def http_json(url, data=None):
    if data is not None:
        body = urllib.parse.urlencode(data).encode()
        req = urllib.request.Request(url, data=body, method="POST", headers={
            "Content-Type": "application/x-www-form-urlencoded",
            "User-Agent": "xllite/3.23.5"})
    else:
        req = urllib.request.Request(url, headers={"User-Agent": "xllite/3.23.5"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read().decode())

def write_state(d):
    os.makedirs(os.path.dirname(STATE), exist_ok=True)
    with open(STATE, "w") as f:
        json.dump(d, f, ensure_ascii=False, indent=2)

def main():
    deadline = time.time() + MAX_RUN
    n = 0
    while time.time() < deadline:
        n += 1
        code = http_json(CODE_URL, {"client_id": CLIENT_ID, "client_secret": CLIENT_SECRET, "scope": SCOPE})
        short = code.get("short_uri_complete", "")
        full = code.get("verification_uri_complete", "")
        dc = code["device_code"]
        write_state({"round": n, "short_url": short, "full_url": full,
                     "user_code": code.get("user_code"), "issued_at": int(time.time()),
                     "expires_in": code.get("expires_in", 120), "status": "waiting_scan"})
        print(f"[round {n}] short={short} (expires {code.get('expires_in',120)}s)", flush=True)
        # 轮询当前码直至授权/过期
        t_end = time.time() + code.get("expires_in", 120)
        while time.time() < t_end:
            time.sleep(2)
            try:
                tok = http_json(TOKEN_URL, {
                    "client_id": CLIENT_ID, "client_secret": CLIENT_SECRET,
                    "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
                    "device_code": dc})
            except Exception as e:
                msg = str(e)
                if "authorization_pending" in msg or "400" in msg or "slow_down" in msg:
                    continue
                if "expired" in msg.lower() or "410" in msg or "gone" in msg.lower():
                    break
                continue
            # 成功
            os.makedirs(HOME, exist_ok=True)
            p = os.path.join(HOME, "auth_token.json")
            with open(p, "w") as f:
                json.dump(tok, f, indent=2)
            os.chmod(p, 0o600)
            os.makedirs(os.path.dirname(ARCH), exist_ok=True)
            with open(ARCH, "w") as f:
                json.dump(tok, f, indent=2)
            os.chmod(ARCH, 0o600)
            write_state({"status": "token_received", "round": n, "received_at": int(time.time()),
                         "access_token_head": str(tok.get("access_token", ""))[:8]})
            print("[+] TOKEN RECEIVED", flush=True)
            return
        write_state({"round": n, "status": "expired_reshuffling", "issued_at": int(time.time())})
    write_state({"status": "daemon_timeout", "rounds": n})

if __name__ == "__main__":
    main()
