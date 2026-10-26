/**
 * BT 面板全路径 e2e（体检 #182 盲区补全：TaskBtPanel 524 行 16 交互零
 * UI 级覆盖——既有 e2e daemon 无 bt feature）。
 *
 * 前置（与 btcore e2e 同基座）：
 *   - daemon 为 --features bt 构建（global-setup 默认二进制路径）
 *   - SEED_MAIN 指向自研 seeder（bt-linux-setup.sh 产物）
 *
 * 覆盖：magnet 添加（UI）→ BT 面板渲染 → peer 注入（UI 输入 + 回执）→
 * 本地 swarm 下载完成（API）→ tracker 刷新 / magnet 复制（剪贴板断言）/
 * 重新校验（回执）。
 */
import { test, expect } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import net from 'node:net';

const BASE = process.env.E2E_BASE_URL!;
const SEED_MAIN =
  process.env.SEED_MAIN || resolve(process.env.HOME || '', 'bt-native/lib/seed_main');

function freePort(): Promise<number> {
  return new Promise((res) => {
    const s = net.createServer();
    s.listen(0, '127.0.0.1', () => {
      const p = (s.address() as { port: number }).port;
      s.close(() => res(p));
    });
  });
}

/** 起 seed_main：输出首行 `SEED <magnet> PORT <port>`。 */
async function startSeeder(): Promise<{ child: ChildProcess; magnet: string; addr: string; dir: string }> {
  const port = await freePort();
  const dir = mkdtempSync(join(tmpdir(), 'sd-bt-e2e-'));
  const child = spawn(SEED_MAIN, [String(port), dir], { stdio: ['ignore', 'pipe', 'pipe'] });
  const line = await new Promise<string>((res, rej) => {
    const t = setTimeout(() => rej(new Error('seeder 15s 未就绪')), 15_000);
    let buf = '';
    child.stdout!.on('data', (d) => {
      buf += d.toString();
      const m = /SEED (\S+) PORT (\d+)/.exec(buf);
      if (m) {
        clearTimeout(t);
        res(m[1]);
      }
    });
    child.on('exit', (c) => rej(new Error(`seeder 退出 code=${c}`)));
  });
  return { child, magnet: line, addr: `127.0.0.1:${port}`, dir };
}

test.describe.serial('BT 面板 e2e（bt daemon + 本地 swarm）', () => {
  let seeder: Awaited<ReturnType<typeof startSeeder>> | null = null;

  test.beforeAll(async () => {
    seeder = await startSeeder();
  });
  test.afterAll(async () => {
    seeder?.child.kill();
  });
  test.afterEach(async ({ request }) => {
    const res = await request.get(`${BASE}/tasks`);
    if (res.ok()) {
      const tasks = (await res.json()) as Array<{ task_id: string }>;
      for (const t of tasks) {
        await request.delete(`${BASE}/tasks/${t.task_id}?delete_data=true`).catch(() => {});
      }
    }
  });

  test('magnet 添加 → 面板渲染 → peer 注入 → 完成 → magnet 复制 / recheck', async ({
    page,
    request,
  }) => {
    test.setTimeout(120_000);
    // 剪贴板读权限（magnet-copy 断言）
    await (page.context() as unknown as { grantPermissions: (p: string[]) => void }).grantPermissions([
      'clipboard-read',
      'clipboard-write',
    ]);

    // 1) UI 添加 magnet 任务
    await page.goto(BASE!);
    await page.getByTestId('new-task-btn').click();
    await page.getByTestId('add-url-input').fill(seeder!.magnet);
    await page.getByTestId('add-submit').click();
    await expect(page.getByTestId('add-url-input')).toBeHidden({ timeout: 15_000 });
    await expect(page.getByTestId('task-row')).toHaveCount(1, { timeout: 20_000 });
    const id = (await (await request.get(`${BASE}/tasks`)).json())[0].task_id as string;

    // 2) 打开详情 → BT 面板渲染（engine=bt）
    await page.getByTestId('task-row').first().dblclick();
    await expect(page.getByLabel('任务详情')).toBeVisible();
    await expect(page.getByText('Peer / Web seed 注入')).toBeVisible({ timeout: 10_000 });

    // 3) UI 注入 seeder peer（输入 + 提交 + 回执 ✓）
    await page.getByTestId('peers-add').fill(seeder!.addr);
    await page.getByTestId('peers-add-submit').click();
    await expect(page.getByLabel('peer 注入回执').locator('.ok').first()).toBeVisible({
      timeout: 10_000,
    });

    // 4) 本地 swarm 下载完成（2MB 秒级）
    const deadline = Date.now() + 90_000;
    for (;;) {
      const t = (await (await request.get(`${BASE}/tasks/${id}`)).json()) as { state: string };
      // D24：完成与做种分离——BT 满块后进 Seeding（做种态即数据完整）
      if (t.state === 'Completed' || t.state === 'Seeding') break;
      if (t.state === 'Failed') throw new Error(`BT 任务失败: ${JSON.stringify(t)}`);
      if (Date.now() > deadline) throw new Error('BT 下载 90s 未完成');
      await new Promise((r) => setTimeout(r, 500));
    }

    // 5) tracker 刷新（回执或空态均可——不崩即接线）
    await page.getByTestId('trackers-refresh').click();

    // 6) magnet 复制 → 剪贴板含 infohash
    await page.getByTestId('magnet-copy').click();
    const clip = await page.evaluate(() => navigator.clipboard.readText());
    expect(clip).toMatch(/^magnet:\?xt=urn:btih:/);

    // 7) 重新校验（需确认：alertdialog「强制重新校验」→ 确认 → 回执）
    await page.getByTestId('recheck-btn').click();
    await expect(page.getByRole('alertdialog')).toBeVisible();
    await page.getByTestId('confirm-ok').click();
    await expect(page.getByText('重新校验已启动').first()).toBeVisible({ timeout: 15_000 });

    // 8) 关闭抽屉不崩
    await page.keyboard.press('Escape');
    await expect(page.getByLabel('任务详情')).toBeHidden();
  });
});
