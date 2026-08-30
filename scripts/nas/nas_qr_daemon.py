#!/usr/bin/env python3
"""xllite 扫码授权守护：循环续发设备码（120s/码），最长运行 30 分钟。
- 每个码发起后立即开始轮询 token（interval=2s）
- 到手 token → 落盘预置路径 + 取证归档 → 退出
- 未授权过期 → 自动发下一个码，状态写 qr_state.json（含最新链接）

授权姿势（2026-08-30 二次实测修正，重要）：
  主路径（网页统一授权页，登录方式全集合）：
  1. 手机/电脑任意浏览器打开本脚本打出的 /yc/ 链接（或扫终端二维码）
  2. 未登录 → 跳官方网页登录页：账密 / 短信验证码 / App 扫码 /
     微信 / QQ / 微博第三方全支持（第三方须已绑定目标迅雷账号：
     App → 设置 → 账号与安全 → 第三方账号绑定）
  3. 登录后出现「远程设备」授权确认 → 点确认 → 2s 内 token 落盘退出
     （页面 POST api-pan.xunlei.com/v1/user/device/authorize；页面 JS
     内置默认 client_id 即本脚本所用 X9ibISwpIp8jQ4Ya + 同款 scope，
     并适配 App/PC/Mac/微信小程序 webview 四端 bridge）
  备路径（App 原生）：App 扫 short_uri 短链（若 App 版本可处理；
  网页浏览器打开短链必 404 —— /__/auth/device/ 页 App-only，实测）
  ⚠ 必须用目标迅雷账号（有云盘权益/VIP 的那个）登录并确认；
    未绑定的第三方会登成另一个账号 → token 授权错身份，A2 校准作废

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
# 网页统一授权页（实测 200 可渲染；/yc/ 页 JS 默认 queryAuth 即本 client_id+scope，
# 未登录自动跳官方网页登录页——账密/短信/扫码/微信/QQ/微博全方式）
WEB_AUTH_BASE = "https://pan.xunlei.com/yc/"

def web_auth_url(user_code):
    """构造 /yc/ 统一授权页链接（scope 显式透传，页面从 URL 参数读取）。"""
    q = urllib.parse.urlencode({"client_id": CLIENT_ID, "user_code": user_code,
                                "scope": SCOPE})
    return WEB_AUTH_BASE + "?" + q
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
    print("=" * 58)
    print(" 迅雷设备授权（网页统一授权页 · 登录方式全集合）")
    print(" 手机/电脑任意浏览器打开下方 /yc/ 链接（或扫二维码）：")
    print("   未登录 → 跳官方网页登录（账密/短信/扫码/微信/QQ/微博）")
    print("   已登录 → 「远程设备」授权确认 → 点确认 → token 自动落盘")
    print(" ⚠ 必须用有云盘权益的目标迅雷账号登录；未绑定的第三方")
    print("   会登成另一个账号 → token 授权错身份，A2 校准作废")
    print(" 备选：手机迅雷 App 扫一扫 short_uri 短链（App 原生确认流）")
    print("=" * 58, flush=True)
    deadline = time.time() + MAX_RUN
    n = 0
    while time.time() < deadline:
        n += 1
        code = http_json(CODE_URL, {"client_id": CLIENT_ID, "client_secret": CLIENT_SECRET, "scope": SCOPE})
        short = code.get("short_uri_complete", "")
        full = code.get("verification_uri_complete", "")
        dc = code["device_code"]
        web = web_auth_url(code.get("user_code", ""))
        write_state({"round": n, "web_auth_url": web, "short_url": short,
                     "full_url": full,
                     "user_code": code.get("user_code"), "issued_at": int(time.time()),
                     "expires_in": code.get("expires_in", 120), "status": "waiting_auth"})
        print(f"[round {n}] 网页授权链接（浏览器打开，全登录方式）:\n  {web}", flush=True)
        print(f"[round {n}] App 短链（备选）: {short} (expires {code.get('expires_in',120)}s)", flush=True)
        render_qr(web)
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
