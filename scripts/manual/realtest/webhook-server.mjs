import http from 'node:http';
import { appendFileSync } from 'node:fs';

/** R8: webhook 接收器 + 同名文件源——POST /hook 记 JSON 行；/small.bin 服务 1MB 确定性内容。 */
const PORT = parseInt(process.argv[2] || '9940', 10);
const LOG = process.argv[3] || '/tmp/webhook-events.jsonl';
const SIZE = 1 * 1024 * 1024;

http.createServer((req, res) => {
  const url = new URL(req.url, `http://x`);
  if (url.pathname === '/hook' && req.method === 'POST') {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      appendFileSync(LOG, body + '\n');
      console.log(`[webhook] ${body.slice(0, 200)}`);
      res.writeHead(200); res.end('ok');
    });
    return;
  }
  if (url.pathname === '/small.bin') {
    const range = req.headers.range;
    if (range) {
      const m = /bytes=(\d*)-(\d*)/.exec(range);
      const start = m[1] ? parseInt(m[1]) : 0;
      const end = m[2] ? Math.min(parseInt(m[2]), SIZE - 1) : SIZE - 1;
      const len = end - start + 1;
      res.writeHead(206, { 'content-type': 'application/octet-stream', 'content-length': String(len), 'accept-ranges': 'bytes' });
      const CH = 64 * 1024;
      for (let i = 0; i < len; i += CH) res.write(Buffer.alloc(Math.min(CH, len - i), (start + i + 7) % 251));
      res.end();
      return;
    }
    res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-length': String(SIZE), 'accept-ranges': 'bytes' });
    const CH = 64 * 1024;
    for (let i = 0; i < SIZE; i += CH) res.write(Buffer.alloc(CH, (i + 7) % 251));
    res.end();
    return;
  }
  res.writeHead(404); res.end('not found');
}).listen(PORT, '127.0.0.1', () => console.log(`webhook+file server :${PORT}, log=${LOG}`));
