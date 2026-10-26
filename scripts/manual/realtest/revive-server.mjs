import http from 'node:http';
import { existsSync } from 'node:fs';

/** R7: 三态服务器——模拟「添加时活 → 下载中死 → 管理员修复复活」的真实源。
 *  用法：node revive-server.mjs <port> <marker-path> <die-after-hits> [size-mb]
 *  /revive.bin 行为：hit ≤ die-after-hits → 200；此后无 marker → 404；有 marker → 200。 */
const PORT = parseInt(process.argv[2] || '9930', 10);
const MARKER = process.argv[3] || '/tmp/revive-now';
const DIE_AFTER = parseInt(process.argv[4] || '2', 10);
const SIZE = parseInt(process.argv[5] || '1', 10) * 1024 * 1024;

let hits = 0;
http.createServer((req, res) => {
  const url = new URL(req.url, `http://x`);
  if (url.pathname === '/revive.bin') {
    hits++;
    const alive = hits <= DIE_AFTER || existsSync(MARKER);
    console.log(`[hit ${hits}] ${alive ? '200' : '404'}`);
    if (!alive) {
      res.writeHead(404, { 'content-type': 'text/plain' });
      res.end('source died');
      return;
    }
    const range = req.headers.range;
    if (range) {
      const m = /bytes=(\d*)-(\d*)/.exec(range);
      const start = m[1] ? parseInt(m[1]) : 0;
      const end = m[2] ? Math.min(parseInt(m[2]), SIZE - 1) : SIZE - 1;
      const len = end - start + 1;
      res.writeHead(206, { 'content-type': 'application/octet-stream', 'content-length': String(len), 'accept-ranges': 'bytes' });
      const CH = 64 * 1024;
      for (let i = 0; i < len; i += CH) res.write(Buffer.alloc(Math.min(CH, len - i), (start + i) % 251));
      res.end();
      return;
    }
    res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-length': String(SIZE), 'accept-ranges': 'bytes' });
    const CH = 64 * 1024;
    for (let i = 0; i < SIZE; i += CH) res.write(Buffer.alloc(CH, i % 251));
    res.end();
    return;
  }
  res.writeHead(404); res.end('not found');
}).listen(PORT, '127.0.0.1', () => console.log(`revive server :${PORT}, die-after=${DIE_AFTER}, marker=${MARKER}`));
