#!/usr/bin/env python3
"""soak 本地确定性文件服务器（stdlib only，无第三方依赖）。

与 ui/e2e/file-server.mjs 同源约定：内容确定性（byteAt(i) = i % 251），
因此任意大小文件的 SHA256 可离线预计算（/sha256 端点），无需先下载一遍。

用途：
  - soak-failure-inject.sh：kill -9 / 断网（SIGSTOP）/ 磁盘写失败场景的受控源
  - soak-http.sh --local：无外网环境（CI 沙箱）下的长跑数据源

端点：
  GET /file?size=<bytes>&throttle=<bytes_per_sec>   确定性内容 + Range + 节流
  GET /sha256?size=<bytes>                          该大小内容的 SHA256（校验基准）

用法：
  python3 soak-file-server.py <port>          # 默认 9903
  curl 'http://127.0.0.1:9903/file?size=1048576&throttle=524288'

设计说明：
  - ThreadingHTTPServer：daemon 分段下载会开多个并发连接，串行服务器会成为瓶颈。
  - Range 支持是硬要求（分段引擎的 206 语义）。
  - 节流按「每 0.1s 补充令牌」实现，粒度足够平滑。
  - SIGSTOP 冻结进程即「断网」（连接僵住无 FIN/RST），SIGCONT 恢复——
    这正是故障注入需要的语义，服务器代码无需感知。
"""
import hashlib
import re
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 9903
CHUNK = 64 * 1024
# 预生成 251*4096 ≈ 1MB 周期模式块；任意偏移的内容 = 该块循环平移
BASE_LEN = 251 * 4096
BASE = bytes(i % 251 for i in range(BASE_LEN))


def content_sha256(size: int) -> str:
    """离线计算确定性内容的 SHA256（不落盘、流式）。"""
    h = hashlib.sha256()
    remaining = size
    offset = 0
    while remaining > 0:
        n = min(remaining, BASE_LEN)
        shift = offset % BASE_LEN
        seg = BASE[shift:] + BASE[:shift] if shift else BASE
        # 需要精确 n 字节：循环块足够大（≥1MB），单次切片足够；n < BASE_LEN
        # 时取前 n 字节即可（内容按全局偏移取模，下一轮 offset 已推进）。
        h.update(seg[:n] if n <= len(seg) else (seg * 2)[:n])
        remaining -= n
        offset += n
    return h.hexdigest()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # 安静模式：故障注入时 stdout 要可读
        if self.headers.get("X-Soak-Verbose"):
            sys.stderr.write("[file-server] " + fmt % args + "\n")

    def _send_head(self, size: int, start: int, end: int):
        partial = (start, end) != (0, size - 1)
        self.send_response(206 if partial else 200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(end - start + 1))
        if partial:
            self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()

    def do_GET(self):
        u = urlparse(self.path)
        if u.path == "/sha256":
            q = parse_qs(u.query)
            size = int(q.get("size", ["0"])[0])
            if size <= 0:
                self.send_error(400, "size required")
                return
            body = content_sha256(size).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if u.path != "/file":
            self.send_error(404)
            return

        q = parse_qs(u.query)
        size = int(q.get("size", ["0"])[0])
        throttle = int(q.get("throttle", ["0"])[0])  # bytes/s，0 = 不限
        if size <= 0:
            self.send_error(400, "size required")
            return

        start, end = 0, size - 1
        rng = self.headers.get("Range")
        if rng:
            m = re.match(r"bytes=(\d*)-(\d*)", rng)
            if m:
                if m.group(1):
                    start = int(m.group(1))
                if m.group(2):
                    end = int(m.group(2))
            if start >= size or start > end:
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{size}")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            end = min(end, size - 1)

        self._send_head(size, start, end)

        pos = start
        budget = 0
        last_refill = time.monotonic()
        try:
            while pos <= end:
                n = min(CHUNK, end - pos + 1)
                if throttle > 0:
                    now = time.monotonic()
                    if now - last_refill >= 0.1:
                        budget += int(throttle * (now - last_refill))
                        last_refill = now
                    if budget <= 0:
                        time.sleep(0.05)
                        continue
                    n = min(n, budget)
                shift = pos % BASE_LEN
                seg = (BASE[shift:] + BASE[:shift])
                take = seg[:n] if n <= len(seg) else (seg * 2)[:n]
                self.wfile.write(take)
                if throttle > 0:
                    budget -= n
                pos += n
        except (BrokenPipeError, ConnectionResetError):
            pass  # 客户端断开（kill -9 场景的正常路径）


if __name__ == "__main__":
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    srv.daemon_threads = True
    print(f"[soak-file-server] listening 127.0.0.1:{PORT}", flush=True)
    srv.serve_forever()
