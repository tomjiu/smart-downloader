#!/usr/bin/env python3
"""xllite 扫码授权守护：循环续发设备码（120s/码），最长运行 30 分钟。
- 每个码发起后立即开始轮询 token（interval=2s）
- 到手 token → 落盘预置路径 + 取证归档 → 退出
- 未授权过期 → 自动发下一个码，状态写 qr_state.json（含最新短链）

扫码姿势（实测 2026-08-30，重要）：
  1. 手机迅雷 App 先登录目标账号（App 自身登录页支持微信/QQ/微博/验证码，
     第三方须已绑定该迅雷账号：App → 设置 → 账号与安全 → 第三方账号绑定）
  2. App → 右上角「扫一扫」→ 扫本脚本打出的终端二维码 → App 内点「确认授权」
     → 2s 内自动收 token 落盘退出；授权环节不再有「选登录方式」页（实测
     /__/auth/device/ 对一切浏览器 UA 均 404，页面只给 App 原生消费）
  ⚠ 必须用目标迅雷账号（有云盘权益/VIP 的那个）确认；未绑定的第三方会
    登成另一个账号 → token 授权错身份，A2 校准作废

路径均可被环境变量覆盖（SD_QR_STATE / SD_QR_HOME / SD_QR_ARCHIVE），
client_id/secret 同理（SD_XL_CLIENT_ID / SD_XL_CLIENT_SECRET，便于轮换）。
可选依赖：pip install qrcode（终端出二维码；未装则提示安装）。
"""
import json, os, sys, time, urllib.request, urllib.parse

try:
    import qrcode
except ImportError:  # 可选依赖：缺省时仅打印链接 + 安装提示
    qrcode = None

# 常量区：引擎内嵌 OAuth 客户端（已随附录 E.2.3 公开；环境变量可覆盖以便轮换）
CLIENT_ID = os.environ.get("SD_XL_CLIENT_ID", "X9ibISwpIp8jQ4Ya")
CLIENT_SECRET = os.environ.get("SD_XL_CLIENT_SECRET", "BlPF2z7HEeutzH4t6zyjLw")
SCOPE = "pan user profile sso offline pan/xunlei/share/create"
CODE_URL = "https://xluser-ssl.xunlei.com/v1/auth/device/code"
TOKEN_URL = "https://xluser-ssl.xunlei.com/v1/auth/token"
# 路径区：默认相对脚本自身定位（任意机器/任意克隆位置均可跑），env 可覆盖
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
STATE = os.environ.get("SD_QR_STATE",
                       os.path.join(SCRIPT_DIR, "qr_state.json"))
HOME = os.environ.get("SD_QR_HOME",
                      os.path.expanduser("~/.nas-engine-test/data/.drive"))
ARCH = os.environ.get("SD_QR_ARCHIVE", os.path.normpath(os.path.join(
    SCRIPT_DIR, "..", "research", "xunlei", "extracted", "cross-platform",
    "xllite_token.json")))
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

def render_qr(url):
    """终端渲染当前轮短链二维码；qrcode 未安装时给出一行安装提示。"""
    if not url:
        return
    if qrcode is None:
        print("  [!] pip install qrcode 后重跑本脚本可在终端直接出二维码", flush=True)
        return
    qr = qrcode.QRCode(border=1)
    qr.add_data(url)
    qr.make()
    qr.print_ascii(invert=True)


def main():
    print("=" * 54)
    print(" 迅雷 App 扫码授权：先在 App 登录好目标账号再扫码")
    print(" App → 扫一扫 → 扫下方二维码 → 确认授权 → token 自动落盘")
    print(" ⚠ 授权环节没有选登录方式的网页（浏览器打开必 404，实测）；")
    print("   微信/QQ/微博登录在 App 自身登录页，且需已绑定目标迅雷账号")
    print("=" * 54, flush=True)
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
        render_qr(short)
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
