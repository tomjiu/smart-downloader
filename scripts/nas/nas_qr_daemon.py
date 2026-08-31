#!/usr/bin/env python3
"""xllite 扫码授权守护（OAuth 2.0 设备码授权 · RFC 8628）：循环续发设备码，最长 2 小时。
- 服务端固定 expires_in=120s/码（不可调），到期自动换新码
- 内置「固定入口」HTTP 服务：随时打开都 302 到最新一轮 /yc/ 授权链接，
  彻底解决「复制/打开链接时码已过期」——实测最高频的失败原因
- 每个码发起后立即开始轮询 token（interval=2s）
- 到手 token → 落盘预置路径 + 取证归档 → 退出
- 状态写 qr_state.json（含最新链接与固定入口）

授权姿势（2026-08-30 三次实测修正，含无头浏览器一手证据）：
  实测事实：
  - /yc/ 是「远程设备」应用（NAS 远程管理/下载台），设备码授权确认面
    就寄生在其中：带 user_code 的 URL = 把一台新远程设备授权进账号
    （RFC 8628 verification_uri；官方 NAS 厂商同款）；/yc/home/ 是登录后
    的设备管理主页——无 user_code 时浏览器落到它，与授权无关，勿误判
  - /yc/ 链接 + 新鲜 user_code（5s 内打开）→ 正常渲染「迅雷-远程设备」页
    →「立即登录」→ i.xunlei.com SSO 登录（手机验证码 / 账号密码 两种方式）
    → 登录完回确认页。user_code 过期（>120s）则报「登录授权过期」。
  - short_uri 短链（api/v1/reurl?action=scan&code=xxx）：新鲜码 302 →
    /__/auth/device/（legacy 页面，浏览器 404）；过期码直接 nginx 404/500。
    浏览器走短链是死路，仅手机迅雷 App 扫码可用（App 端原生处理）。
  推荐姿势（最稳）：
  1. 常用浏览器先正常登录 pan.xunlei.com（主站登录页方式全集合：App 扫码 /
     微信 / QQ / 微博 / 账密 / 短信；第三方须已绑定目标账号）——SSO 登录态全域共享
  2. 跑本脚本，浏览器打开「固定入口」（随时打开=最新码）→ 已登录则直接出现
     「远程设备」确认 → 点确认 → token 自动落盘
  未登录姿势：打开固定入口 →「立即登录」→ 手机验证码或账密登录；若登录耗时
  超过 120s 导致确认页报「授权过期」，重新打开固定入口即可（已登录态秒级确认）
  ⚠ 必须用目标迅雷账号（有云盘权益/VIP 的那个）登录并确认；
    未绑定的第三方会登成另一个账号 → token 授权错身份，A2 校准作废

路径均可被环境变量覆盖（SD_QR_STATE / SD_QR_HOME / SD_QR_ARCHIVE），
固定入口（SD_QR_HTTP_PORT 默认 8162、0=关闭；SD_QR_HTTP_BIND 默认 0.0.0.0
供局域网手机访问），client_id/secret 同理（SD_XL_CLIENT_ID / SD_XL_CLIENT_SECRET，
便于轮换）。
固定入口仅限本机/局域网使用，请勿暴露公网（持有链接者可为该设备会话授权）。
可选依赖：pip install qrcode（终端出二维码；未装则提示安装）。
"""
import json, os, socket, sys, threading, time, urllib.request, urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

try:
    import qrcode
except ImportError:  # 可选依赖：缺省时仅打印链接 + 安装提示
    qrcode = None

# ── 常量区：引擎内嵌 OAuth 客户端（环境变量注入，无明文 fallback）
CLIENT_ID = os.environ["SD_XL_CLIENT_ID"]
CLIENT_SECRET = os.environ["SD_XL_CLIENT_SECRET"]
SCOPE = "pan user profile sso offline pan/xunlei/share/create"
CODE_URL = "https://xluser-ssl.xunlei.com/v1/auth/device/code"
TOKEN_URL = "https://xluser-ssl.xunlei.com/v1/auth/token"
# 网页统一授权页（实测 200 可渲染；未登录跳 i.xunlei.com SSO：手机验证码/账密）
WEB_AUTH_BASE = "https://pan.xunlei.com/yc/"

# ── 固定入口：内置 HTTP 服务，把「随时打开」翻译成「最新一轮 /yc/ 链接」
HTTP_PORT = int(os.environ.get("SD_QR_HTTP_PORT", "8162"))
HTTP_BIND = os.environ.get("SD_QR_HTTP_BIND", "0.0.0.0")

_cur = {"web": "", "round": 0, "expires_at": 0.0, "expires_in": 120}
_cur_lock = threading.Lock()


def _set_cur(web, round_no, expires_in):
    with _cur_lock:
        _cur.update(web=web, round=round_no,
                    expires_at=time.time() + expires_in, expires_in=expires_in)


def _get_cur():
    with _cur_lock:
        return dict(_cur)


class _EntryHandler(BaseHTTPRequestHandler):
    """GET / → 最新码 302；码将过期(<10s)或未就绪 → 短暂自动刷新提示页。"""

    def do_GET(self):
        cur = _get_cur()
        left = cur["expires_at"] - time.time()
        if cur["web"] and left >= 10:
            self.send_response(302)
            self.send_header("Location", cur["web"])
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if not cur["web"]:
            wait, msg = 2, "设备码就绪中，{} 秒后自动进入授权页…".format(2)
        else:
            wait = max(2, min(10, int(left) + 2))
            msg = "本轮码剩 {:0.0f}s 即将换新，{} 秒后自动进入最新授权页…".format(
                max(left, 0), wait)
        body = ('<!doctype html><meta charset="utf-8">'
                '<title>迅雷设备授权入口</title>'
                '<meta http-equiv="refresh" content="{}">'
                '<p style="font:16px/1.8 sans-serif;padding:2em">{}</p>'
                .format(wait, msg))
        data = body.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):  # 静默访问日志
        pass


def start_entry_server():
    """固定入口线程；端口不可用/被关闭时返回 None（回退为仅每轮打印链接）。"""
    if HTTP_PORT <= 0:
        return None
    try:
        srv = ThreadingHTTPServer((HTTP_BIND, HTTP_PORT), _EntryHandler)
    except OSError as e:
        print("[!] 固定入口端口 {} 不可用（{}）→ 回退：仅用每轮打印的 /yc/ 链接"
              .format(HTTP_PORT, e), flush=True)
        return None
    threading.Thread(target=srv.serve_forever, daemon=True,
                     name="qr-entry").start()
    return srv


def lan_ip():
    """探测局域网出口 IP（UDP connect 不发流量）；失败回退 127.0.0.1。"""
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.settimeout(0)
        s.connect(("223.5.5.5", 80))
        ip = s.getsockname()[0]
        s.close()
        return ip
    except Exception:
        try:
            return socket.gethostbyname(socket.gethostname())
        except Exception:
            return "127.0.0.1"


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
    """终端渲染二维码；qrcode 未安装时给出一行安装提示。"""
    if not url or qrcode is None:
        if qrcode is None:
            print("  [!] pip install qrcode 后重跑本脚本可在终端直接出二维码",
                  flush=True)
        return
    qr = qrcode.QRCode(border=1)
    qr.add_data(url)
    qr.make()
    qr.print_ascii(invert=True)


def main():
    ip = lan_ip()
    entry = None
    if HTTP_PORT > 0:
        entry = "http://{}:{}/".format(
            ip if HTTP_BIND in ("0.0.0.0", "") else HTTP_BIND, HTTP_PORT)
    print("=" * 62)
    print(" 迅雷设备授权（OAuth 2.0 设备码 · 网页统一授权页）")
    if entry:
        print(" 固定入口（推荐；随时打开都指向最新一轮码）:")
        print("   " + entry)
    print(" 注：/yc/ 是「远程设备」应用，授权确认就在其中；若浏览器落到")
    print(" /yc/home/ 设备管理页，说明链接丢了 user_code——回固定入口重开即可")
    print(" 推荐姿势：浏览器先登录 pan.xunlei.com（主站登录页含 App 扫码/")
    print(" 微信/QQ/微博/账密/短信全方式）→ 打开固定入口 →「远程设备」")
    print(" 确认 → token 自动落盘。未登录则走「立即登录」（手机验证码/账密），")
    print(" 登录后若提示授权过期，重开固定入口即秒级确认。")
    print(" ⚠ 必须用有云盘权益的目标迅雷账号；未绑定的第三方会登成")
    print("   另一个账号 → token 授权错身份，A2 校准作废")
    print(" 备选：手机迅雷 App 扫 short_uri 短链（App 原生确认流；")
    print("   ⚠ 浏览器打开短链必 404/500——302 落点 legacy 页已下线，属正常）")
    print("=" * 62, flush=True)
    srv = start_entry_server()
    if srv and entry:
        render_qr(entry)  # 固定入口二维码：扫了永远进最新码
    deadline = time.time() + MAX_RUN
    n = 0
    while time.time() < deadline:
        n += 1
        code = http_json(CODE_URL, {"client_id": CLIENT_ID, "client_secret": CLIENT_SECRET, "scope": SCOPE})
        short = code.get("short_uri_complete", "")
        full = code.get("verification_uri_complete", "")
        dc = code["device_code"]
        expires_in = code.get("expires_in", 120)
        web = web_auth_url(code.get("user_code", ""))
        _set_cur(web, n, expires_in)
        write_state({"round": n, "web_auth_url": web, "short_url": short,
                     "full_url": full, "entry_url": entry or "",
                     "user_code": code.get("user_code"), "issued_at": int(time.time()),
                     "expires_in": expires_in, "status": "waiting_auth"})
        print(f"[round {n}] 新码已发（{expires_in}s 内有效，过期自动换新）", flush=True)
        print(f"[round {n}] 网页授权链接: {web}", flush=True)
        print(f"[round {n}] App 短链（仅 App 扫码，浏览器打开 404/500 属正常）: {short}", flush=True)
        if not srv:
            render_qr(web)  # 无固定入口时，直接出当前轮链接二维码
        # 轮询当前码直至授权/过期
        t_end = time.time() + expires_in
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
            print("[+] TOKEN RECEIVED（固定入口随进程退出）", flush=True)
            return
        write_state({"round": n, "status": "expired_reshuffling", "issued_at": int(time.time())})
    write_state({"status": "daemon_timeout", "rounds": n})


if __name__ == "__main__":
    main()
