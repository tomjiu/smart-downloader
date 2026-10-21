/**
 * Playwright global setup：起真实 daemon（--ui-dir 服务新 UI）+ 本地限流
 * 文件服务器，测完统一收尾。
 *
 * 环境约定：
 *   SD_DAEMON_BIN  daemon 二进制路径（默认 <repo>/target/debug/smart-dl-daemon）
 *   E2E_DAEMON_PORT  默认 8788；E2E_FILE_PORT 默认 9902
 *   ui/out 必须已构建（bun run build）
 *
 * 暴露给测试：E2E_BASE_URL（页面+API 同源基址）、E2E_FILE_BASE（文件服务器）。
 */
import { spawn, type ChildProcess } from 'node:child_process';
import { existsSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const UI_DIR = resolve(__dirname, '..'); // ui/
const REPO = resolve(UI_DIR, '..');
const DAEMON_BIN = process.env.SD_DAEMON_BIN || join(REPO, 'target/debug/smart-dl-daemon');
const DAEMON_PORT = parseInt(process.env.E2E_DAEMON_PORT || '8788', 10);
const FILE_PORT = parseInt(process.env.E2E_FILE_PORT || '9902', 10);

let daemon: ChildProcess | null = null;
let fileServer: ChildProcess | null = null;
let workDir = '';

async function waitHealthy(url: string, timeoutMs: number, what: string) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(url);
      if (r.ok) return;
    } catch {
      /* 未就绪 */
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`${what} 在 ${timeoutMs}ms 内未就绪（${url}）`);
}

export default async function globalSetup(): Promise<() => Promise<void>> {
  // 0. 前置检查：UI 产物 + daemon 二进制
  if (!existsSync(join(UI_DIR, 'out', 'index.html'))) {
    throw new Error(
      `ui/out/index.html 不存在——先在 ui/ 下执行 bun run build（e2e 走真实产物，不用 dev server）`,
    );
  }
  if (!existsSync(DAEMON_BIN)) {
    throw new Error(
      `daemon 二进制不存在（${DAEMON_BIN}）——先 cargo build -p smart-dl-daemon，或用 SD_DAEMON_BIN 指定路径`,
    );
  }

  // 1. 独立工作目录（配置/落盘互不污染宿主环境）
  workDir = mkdtempSync(join(tmpdir(), 'sd-e2e-'));
  const configPath = join(workDir, 'config.toml');
  writeFileSync(
    configPath,
    [
      '[server]',
      `addr = "127.0.0.1:${DAEMON_PORT}"`,
      '',
      '[download]',
      `dest_root = "${join(workDir, 'downloads').replace(/\\/g, '/')}"`,
      // e2e 用默认（每段 fsync）：崩溃一致性最严口径下跑完全部路径
      'fsync_per_segment = true',
      '',
    ].join('\n'),
  );

  // 2. 文件服务器（确定性内容 + 限流；见 file-server.mjs 头注释）
  fileServer = spawn(process.execPath, [join(__dirname, 'file-server.mjs'), String(FILE_PORT)], {
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  fileServer.stdout!.on('data', () => {});
  fileServer.stderr!.on('data', (d: Buffer) => console.error('[file-server]', d.toString()));

  // 3. daemon（服务真实 UI 产物 + 真实下载引擎）
  daemon = spawn(
    DAEMON_BIN,
    [
      'serve',
      '--config',
      configPath,
      '--ui-dir',
      join(UI_DIR, 'out'),
      '--addr',
      `127.0.0.1:${DAEMON_PORT}`,
    ],
    { stdio: ['ignore', 'pipe', 'pipe'], cwd: workDir },
  );
  daemon.stdout!.on('data', () => {});
  daemon.stderr!.on('data', (d: Buffer) => console.error('[daemon]', d.toString().trim()));

  const base = `http://127.0.0.1:${DAEMON_PORT}`;
  const fileBase = `http://127.0.0.1:${FILE_PORT}`;
  // 文件服务器无 /health：Range 0-0 探测（1 字节立即返回）
  await waitHealthy(
    `${fileBase}/small.bin`,
    8000,
    '文件服务器',
  ).catch(async () => {
    // waitHealthy 用 r.ok 判定；部分实现对 206 也算 ok，兜底再显式探测一次
    const r = await fetch(`${fileBase}/small.bin`, { headers: { Range: 'bytes=0-0' } });
    if (!(r.ok || r.status === 206)) throw new Error('文件服务器未就绪');
  });
  await waitHealthy(`${base}/health`, 15000, 'daemon /health');

  process.env.E2E_BASE_URL = base;
  process.env.E2E_FILE_BASE = fileBase;

  // 返回 teardown
  return async () => {
    for (const p of [daemon, fileServer]) {
      if (p && p.exitCode == null) {
        p.kill('SIGTERM');
      }
    }
    // 给进程优雅退出的时间（daemon SIGTERM 优雅关闭是 C-2 验证过的语义）
    await new Promise((r) => setTimeout(r, 500));
    for (const p of [daemon, fileServer]) {
      if (p && p.exitCode == null) p.kill('SIGKILL');
    }
    daemon = null;
    fileServer = null;
    console.log(`e2e teardown 完成（工作目录 ${workDir} 留存于系统 tmp，随系统清理）`);
  };
}
