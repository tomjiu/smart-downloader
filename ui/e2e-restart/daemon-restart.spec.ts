/**
 * R18 daemon 重启韧性 e2e（独立套件——自带 daemon 生命周期，不入主套件）：
 * SSE live 流在 daemon 重启（seq 归零）后必须自愈。
 *
 * 缺陷背景（R18 排查发现，R16 live 边界的边界）：
 *   daemon 重启 → WsHub seq 从 0 重计 → 页面陈旧 lastSeq（如 5000）>
 *   新最大 seq → SSE after=5000 永远滤空——live 流静默死亡（无事件、
 *   无报错、无 gap 提示），速率列/完成 toast/日志全部失效直到手动刷新。
 *
 * 修复（store.ts 重连路径）：onerror → 重连前 GET /events?limit=1 探测
 * latest_seq；探测到回退（latest < lastSeq）→ lastSeq 归零全量重放。
 *
 * 验证路径：开页（事件流工作：toast 出现）→ kill -9 daemon → 重启
 * （同端口同配置）→ 等重连 → 新任务完成 → toast 必须再次出现。
 *
 * 运行（与主套件隔离，专用 config）：
 *   cd ui && npx playwright test -c e2e-restart/playwright.config.ts
 */
import { test, expect } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { existsSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const PORT = 8791;
const FILE_PORT = 9904;
const BASE = `http://127.0.0.1:${PORT}`;
const FILE = `http://127.0.0.1:${FILE_PORT}`;
const UI_DIR = resolve(__dirname, '..');
const REPO = resolve(UI_DIR, '..');
const DAEMON_BIN = process.env.SD_DAEMON_BIN || join(REPO, 'target/debug/smart-dl-daemon');

let daemon: ChildProcess | null = null;
let fileServer: ChildProcess | null = null;
let workDir = '';
let configPath = '';

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

/** 起一个 daemon（同 workDir/配置——重启语义 = 同一实例的进程级重启）。 */
async function startDaemon() {
  if (!existsSync(DAEMON_BIN)) throw new Error(`daemon 二进制不存在: ${DAEMON_BIN}`);
  daemon = spawn(DAEMON_BIN, ['serve', '--config', configPath, '--ui-dir', join(UI_DIR, 'out')], {
    stdio: ['ignore', 'pipe', 'pipe'],
    cwd: workDir,
  });
  daemon.stdout!.on('data', () => {});
  daemon.stderr!.on('data', (d: Buffer) => console.error('[daemon]', d.toString().trim()));
  await waitHealthy(`${BASE}/health`, 15000, 'daemon /health');
}

test.beforeAll(async () => {
  workDir = mkdtempSync(join(tmpdir(), 'sd-restart-'));
  configPath = join(workDir, 'config.toml');
  writeFileSync(
    configPath,
    [
      '[server]',
      `addr = "127.0.0.1:${PORT}"`,
      '',
      '[download]',
      `dest_root = "${join(workDir, 'downloads').replace(/\\/g, '/')}"`,
      '',
    ].join('\n'),
  );
  fileServer = spawn(process.execPath, [join(UI_DIR, 'e2e', 'file-server.mjs'), String(FILE_PORT)], {
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  fileServer.stdout!.on('data', () => {});
  await startDaemon();
});

test.afterAll(async () => {
  daemon?.kill('SIGKILL');
  fileServer?.kill('SIGKILL');
});

test('daemon kill -9 重启后 SSE live 流自愈（seq 归零不静默死亡）', async ({ page, request }) => {
  test.setTimeout(120_000);

  // ---- 1. 重启前：live 流工作 + 制造大 seq 头寸 ----
  // 头寸是红路径确定性的关键：40 任务风暴 → 数百事件 → 页面 lastSeq 高；
  // 重启后单任务 ~10 事件追不上（无修复时 completed 永远滤掉 → 无 toast）。
  // （首轮验证教训：1 任务 ~20 事件头寸太小，新事件几秒追上——伪绿。）
  await page.goto(BASE);
  for (let i = 1; i <= 40; i++) {
    const r = await request.post(`${BASE}/tasks`, {
      data: { url: `${FILE}/bulk/${i}.bin`, name: `pre-${String(i).padStart(2, '0')}.bin` },
    });
    expect(r.ok()).toBeTruthy();
  }
  // live 流工作证据：完成 toast 出现（40 任务完成时页面开着）
  await expect(page.getByText('任务完成').first()).toBeVisible({ timeout: 20_000 });
  // 等全部完成（事件头寸落定）+ toast 退场
  await expect
    .poll(
      async () => {
        const tasks = (await (await request.get(`${BASE}/tasks`)).json()) as Array<{
          state: string;
        }>;
        return tasks.every((t) => t.state === 'Completed' || t.state === 'Seeding');
      },
      { timeout: 30_000 },
    )
    .toBe(true);
  // 记录当前最大 seq（头部证据；重启后归零对照）
  const preSeq = (
    (await (
      await request.get(`${BASE}/events?limit=1`)
    ).json()) as { latest_seq: number }
  ).latest_seq;
  // 40 任务 × ~4 事件 ≈ 160；单任务重启后 ~10-15 事件——10 倍余量即确定性
  expect(
    preSeq,
    '重启前 seq 头寸必须足够大（红路径确定性）',
  ).toBeGreaterThan(100);
  await page.waitForTimeout(4500); // toast 退场

  // ---- 2. kill -9 daemon（页面保持开着）----
  daemon?.kill('SIGKILL');
  await new Promise((r) => setTimeout(r, 500));

  // ---- 3. 重启 daemon（同端口同配置；seq 归零）----
  await startDaemon();

  // ---- 4. 自愈验证：新任务完成 → toast 必须再次出现 ----
  // （无修复时：SSE after=陈旧seq 永远滤空 → 无 toast → 本断言超时失败）
  const r2 = await request.post(`${BASE}/tasks`, {
    data: { url: `${FILE}/delete.bin`, name: 'post-restart.bin' },
  });
  expect(r2.ok()).toBeTruthy();
  await expect(page.getByText('任务完成').first()).toBeVisible({ timeout: 20_000 });

  // ---- 5. 列表轮询同样恢复（任务行出现）----
  await expect(page.getByTestId('task-row').first()).toBeVisible({ timeout: 15_000 });
});
