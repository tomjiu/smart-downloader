/**
 * e2e 本地文件服务器：确定性内容（i % 251）+ Range 支持 + 按路径限流。
 *
 * 零磁盘占用（内容按公式现算）；限流让「daemon 侧任务限速」成为唯一
 * 可测瓶颈（服务器恒快于限速值时，UI 速率显示即 daemon limiter 速率）。
 *
 * 用法：node e2e/file-server.mjs <port>
 * 端点（大小 / 限流）：
 *   /small.bin   1MB   不限      —— 完成路径（快速走完）
 *   /limit.bin   128MB 4 MiB/s   —— 限速路径（无限制时 ~16s 完成；
 *                                    限速落地窗口充足，任务限速器成为瓶颈）
 *   /pause.bin   32MB  4 MiB/s   —— 暂停/恢复（16MB 段 ≈ 4s，段边界暂停可测）
 *   /delete.bin  8MB   512 KiB/s —— 删除路径（删除时仍在下载中）
 *
 * 注：段粒度 16MB（DEFAULT_MIN_SPLIT）——进度/速率只在段完成时推进，
 * 速率列在段间呈突发（0/尖峰交替），不能作为限速断言的稳定信号；
 * 限速断言用「状态 + 进度 + API 回显」组合（见 critical-paths.spec.ts #2）。
 */
import http from 'node:http';

const PORT = parseInt(process.argv[2] || '9902', 10);
const MiB = 1024 * 1024;
const KiB = 1024;

const FILES = {
  '/small.bin': { size: 1 * MiB, throttle: 0 },
  '/limit.bin': { size: 128 * MiB, throttle: 4 * MiB },
  '/pause.bin': { size: 32 * MiB, throttle: 4 * MiB },
  '/delete.bin': { size: 8 * MiB, throttle: 512 * KiB },
};

// R16 批量负载路由：任意 /bulk/<name> 服务 64KiB 确定性内容——
// 重负载 e2e 需要大量互异 URL（canonical 去重按 URL 判异），固定四文件
// 不够用；catch-all 免去逐文件登记。
const BULK_SIZE = 64 * KiB;

function resolveFile(pathname) {
  const f = FILES[pathname];
  if (f) return f;
  if (pathname.startsWith('/bulk/')) return { size: BULK_SIZE, throttle: 0 };
  return null;
}

const byteAt = (i) => i % 251;

const server = http.createServer((req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  const f = resolveFile(url.pathname);
  if (!f) {
    res.writeHead(404, { 'Content-Type': 'text/plain' });
    res.end('not found');
    return;
  }
  let start = 0;
  let end = f.size - 1;
  let partial = false;
  const range = req.headers.range;
  if (range) {
    const m = /bytes=(\d*)-(\d*)/.exec(range);
    if (m) {
      if (m[1]) start = parseInt(m[1], 10);
      if (m[2]) end = parseInt(m[2], 10);
      partial = true;
    }
  }
  if (start >= f.size || start > end) {
    res.writeHead(416, { 'Content-Range': `bytes */${f.size}` });
    res.end();
    return;
  }
  end = Math.min(end, f.size - 1);
  const headers = {
    'Content-Type': 'application/octet-stream',
    'Accept-Ranges': 'bytes',
    'Content-Length': String(end - start + 1),
  };
  if (partial) headers['Content-Range'] = `bytes ${start}-${end}/${f.size}`;
  res.writeHead(partial ? 206 : 200, headers);

  // 限流写：16KB 块 × 按速率间隔发送；不限流时连续写。
  const CHUNK = 16 * 1024;
  const chunkDelayMs = f.throttle > 0 ? (CHUNK / f.throttle) * 1000 : 0;
  let pos = start;
  const buf = Buffer.allocUnsafe(CHUNK);
  function writeChunk() {
    if (res.destroyed || res.writableEnded) return;
    if (pos > end) {
      res.end();
      return;
    }
    const n = Math.min(CHUNK, end - pos + 1);
    for (let i = 0; i < n; i++) buf[i] = byteAt(pos + i);
    res.write(Buffer.from(buf.subarray(0, n)));
    pos += n;
    if (chunkDelayMs > 0) setTimeout(writeChunk, chunkDelayMs);
    else {
      // 不限流：setImmediate 让 IO 事件循环喘息，避免大响应饿死其它连接
      setImmediate(writeChunk);
    }
  }
  writeChunk();
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`e2e file-server listening on http://127.0.0.1:${PORT}`);
});
