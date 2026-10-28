/**
 * R24 daemon 不可达 UX e2e：首跑时序（UI 先于 daemon 启动，或地址配错）
 * 此前只见无限骨架条——用户无法区分「慢加载」与「daemon 未启动」，
 * 唯一线索是侧栏小圆点。修复后的完整用户旅程：
 *
 *   1. DD-1 首轮决出且双请求全败 → DaemonDown 面板（明确原因 + 地址 +
 *      令牌 + 自愈提示），骨架不再无限渲染；
 *   2. DD-2 自定义地址/令牌在面板如实呈现（排障第一手信息）；
 *   3. DD-3 健康路径零闪变：首拍请求在飞 = 骨架（与 SSR HTML 一致），
 *      面板绝不出现（MutationObserver 全程监视，防「每次加载闪红」回归）；
 *   4. DD-4 真实自愈：面板挂着 → daemon 起来（同端口）→ 零交互进入
 *      空态任务列表 + 侧栏已连接；再杀 daemon → 已加载数据保留
 *      （P1-4 失败保旧数据语义）+ 侧栏翻回未连接——区分「从未连上」
 *      与「连上后断连」两态。
 *
 *   5. DD-5 令牌配错（HTTP 401，daemon 活着）→ 认证失败面板变体
 *      （R25：HttpError 分类——与「无法连接」分道，指引检查令牌/白名单）；
 *   6. DD-6 设置页修正令牌 → 零重载自愈（headers() 每请求读 localStorage）。
 *
 * 死端口配方：127.0.0.1:1 特权端口确定性 refused（#174 同配方，无
 * 端口复用竞态）；恢复端口 8795/8797 独占（主套件 8788 / restart 套件
 * 8791 之外），自带 daemon 生命周期（e2e-restart 同模式），与共享 daemon
 * 零状态交集。
 */
import { test, expect, type Page } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import { existsSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const DEAD_BASE = 'http://127.0.0.1:1';
const PORT = 8795;
const BASE = `http://127.0.0.1:${PORT}`;
const AUTH_PORT = 8797;
const AUTH_BASE = `http://127.0.0.1:${AUTH_PORT}`;
const UI_DIR = resolve(__dirname, '..');
const REPO = resolve(UI_DIR, '..');
const DAEMON_BIN = process.env.SD_DAEMON_BIN || join(REPO, 'target/debug/smart-dl-daemon');

let daemon: ChildProcess | null = null;
let workDir = '';
let configPath = '';

/** 指定 daemon 地址（可选令牌）加载页面——模拟用户配置了错误/未启动的地址。 */
async function gotoWithBase(page: Page, base: string, token?: string) {
  await page.addInitScript(
    ([b, t]) => {
      localStorage.setItem('dl.daemon.base', b);
      if (t) localStorage.setItem('dl.daemon.token', t);
      else localStorage.removeItem('dl.daemon.token');
    },
    [base, token],
  );
  await page.goto('/');
}

test.describe('daemon 不可达 UX', () => {
  test.afterAll(async () => {
    daemon?.kill('SIGKILL');
  });

  test('DD-1 daemon 未启动：明确面板替代无限骨架（地址/令牌/自愈提示）', async ({ page }) => {
    await gotoWithBase(page, DEAD_BASE);
    const panel = page.getByTestId('daemon-down');
    // 首轮 1s 轮询决出后面板出现
    await expect(panel).toBeVisible({ timeout: 10_000 });
    await expect(panel.getByText('无法连接 daemon')).toBeVisible();
    // 地址如实呈现（排障第一手信息）
    await expect(page.getByTestId('daemon-down-base')).toHaveText(DEAD_BASE);
    // 未设令牌 → 「未设置」
    await expect(panel.getByText('未设置')).toBeVisible();
    // 骨架不再渲染（旧体验 = 无限灰条）
    await expect(page.locator('.dl-skel-hint')).toHaveCount(0);
    // 侧栏同步未连接
    await expect(page.locator('.dl-conn.is-err')).toContainText('未连接');
    // 自愈提示在
    await expect(panel.getByText(/自动重试/)).toBeVisible();
  });

  test('DD-2 自定义地址 + Bearer 令牌在面板如实呈现', async ({ page }) => {
    await gotoWithBase(page, DEAD_BASE, 'tok-xyz');
    const panel = page.getByTestId('daemon-down');
    await expect(panel).toBeVisible({ timeout: 10_000 });
    await expect(page.getByTestId('daemon-down-base')).toHaveText(DEAD_BASE);
    await expect(panel.getByText('已设置（Bearer）')).toBeVisible();
  });

  test('DD-3 健康路径零闪变：首拍在飞 = 骨架，面板绝不出现', async ({ page }) => {
    // MutationObserver 全程监视：面板哪怕一帧出现都算回归
    // （静态 HTML 不含面板——它只会经客户端渲染出现，观察器必捕获）。
    await page.addInitScript(() => {
      (window as unknown as { __ddSeen: boolean }).__ddSeen = false;
      const obs = new MutationObserver(() => {
        if (document.querySelector('[data-testid="daemon-down"]')) {
          (window as unknown as { __ddSeen: boolean }).__ddSeen = true;
        }
      });
      obs.observe(document.documentElement, { childList: true, subtree: true });
    });
    await page.goto('/');
    // 正常进入任务界面（空态或表格，取决于共享 daemon 状态）
    await expect(page.locator('.dl-conn.is-ok')).toContainText('已连接', { timeout: 15_000 });
    // 整个加载过程面板从未出现
    expect(await page.evaluate(() => (window as unknown as { __ddSeen: boolean }).__ddSeen)).toBe(false);
    await expect(page.getByTestId('daemon-down')).toHaveCount(0);
  });

  test('DD-4 真实自愈：daemon 起来零交互恢复；再断连保留已加载数据', async ({ page }) => {
    test.setTimeout(120_000);
    if (!existsSync(DAEMON_BIN)) throw new Error(`daemon 二进制不存在: ${DAEMON_BIN}`);

    // ---- 1. 页面先于 daemon 启动（真实首跑时序）→ 面板 ----
    await gotoWithBase(page, BASE);
    const panel = page.getByTestId('daemon-down');
    await expect(panel).toBeVisible({ timeout: 10_000 });

    // ---- 2. daemon 在面板挂着时启动（同端口） ----
    workDir = mkdtempSync(join(tmpdir(), 'sd-ddown-'));
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
    daemon = spawn(DAEMON_BIN, ['serve', '--config', configPath, '--ui-dir', join(UI_DIR, 'out')], {
      stdio: ['ignore', 'pipe', 'pipe'],
      cwd: workDir,
    });
    daemon.stdout!.on('data', () => {});
    daemon.stderr!.on('data', (d: Buffer) => console.error('[dd-daemon]', d.toString().trim()));
    // 等 /health 就绪
    const deadline = Date.now() + 15_000;
    for (;;) {
      try {
        const r = await fetch(`${BASE}/health`);
        if (r.ok) break;
      } catch {
        /* 未就绪 */
      }
      if (Date.now() > deadline) throw new Error('恢复 daemon 15s 未就绪');
      await new Promise((r) => setTimeout(r, 200));
    }

    // ---- 3. 零交互自愈：面板消失 → 空态列表 + 侧栏已连接 ----
    // （专属 workDir → 零任务 → 「暂无任务」空态，不用等任何数据）
    await expect(page.getByText('暂无任务')).toBeVisible({ timeout: 15_000 });
    await expect(panel).toHaveCount(0);
    await expect(page.locator('.dl-conn.is-ok')).toContainText('已连接');

    // ---- 4. 再断连：loaded 已真 → 保留已加载数据（P1-4 失败保旧数据），
    //         侧栏翻回未连接——「从未连上」（面板）与「断连」（旧数据）
    //         两态区分的回归护栏 ----
    daemon.kill('SIGKILL');
    daemon = null;
    await expect(page.locator('.dl-conn.is-err')).toContainText('未连接', { timeout: 15_000 });
    // 空态仍在（数据未被清成面板/骨架）
    await expect(page.getByText('暂无任务')).toBeVisible();
    await expect(page.getByTestId('daemon-down')).toHaveCount(0);
  });

  test('DD-5 令牌配错（401，daemon 活着）→ 认证失败面板变体（非「无法连接」）', async ({ page }) => {
    test.setTimeout(120_000);
    if (!existsSync(DAEMON_BIN)) throw new Error(`daemon 二进制不存在: ${DAEMON_BIN}`);

    // token 模式 daemon（先启动——本测验证的是「可达但拒绝」）
    const wd = mkdtempSync(join(tmpdir(), 'sd-ddauth-'));
    const cfg = join(wd, 'config.toml');
    writeFileSync(
      cfg,
      [
        '[server]',
        `addr = "127.0.0.1:${AUTH_PORT}"`,
        'http_token = "real-tok"',
        '',
        '[download]',
        `dest_root = "${join(wd, 'downloads').replace(/\\/g, '/')}"`,
        '',
      ].join('\n'),
    );
    const authDaemon = spawn(
      DAEMON_BIN,
      ['serve', '--config', cfg, '--ui-dir', join(UI_DIR, 'out')],
      { stdio: ['ignore', 'pipe', 'pipe'], cwd: wd },
    );
    authDaemon.stdout!.on('data', () => {});
    authDaemon.stderr!.on('data', (d: Buffer) => console.error('[dd-auth]', d.toString().trim()));
    try {
      // token 模式下 /health 同样要求 Bearer（auth_mw 覆盖全端点）
      const deadline = Date.now() + 15_000;
      for (;;) {
        try {
          const r = await fetch(`${AUTH_BASE}/health`, {
            headers: { Authorization: 'Bearer real-tok' },
          });
          if (r.ok) break;
        } catch {
          /* 未就绪 */
        }
        if (Date.now() > deadline) throw new Error('auth daemon 15s 未就绪');
        await new Promise((r) => setTimeout(r, 200));
      }

      // 页面带错误令牌 → 401 → 认证失败变体（不是「无法连接 daemon」）
      await gotoWithBase(page, AUTH_BASE, 'wrong-tok');
      const panel = page.getByTestId('daemon-down');
      await expect(panel).toBeVisible({ timeout: 10_000 });
      await expect(panel).toHaveAttribute('data-mode', 'auth');
      await expect(panel.getByText('认证失败（daemon 拒绝了请求）')).toBeVisible();
      await expect(panel.getByText('无法连接 daemon')).toHaveCount(0);
      // 地址/令牌行如实呈现（排障第一手）
      await expect(page.getByTestId('daemon-down-base')).toHaveText(AUTH_BASE);
      await expect(panel.getByText('已设置（Bearer）')).toBeVisible();
      // 指引面向令牌/白名单（非「请确认 daemon 已启动」）
      await expect(panel.getByText(/http_token/)).toBeVisible();

      // ---- DD-6：设置页修正令牌 → 零重载自愈（真实用户路径：headers()
      //      每请求读 localStorage，下一拍轮询即用新令牌；面板消失） ----
      await page.getByRole('button', { name: '设置' }).click();
      const tokInput = page.locator('#set-a11y-25');
      await expect(tokInput).toBeVisible();
      await tokInput.fill('real-tok');
      // 回任务视图（轮询期间已在自愈）
      await page.getByRole('button', { name: '全部' }).click();
      await expect(page.getByText('暂无任务')).toBeVisible({ timeout: 15_000 });
      await expect(page.locator('.dl-conn.is-ok')).toContainText('已连接');
      await expect(page.getByTestId('daemon-down')).toHaveCount(0);
    } finally {
      authDaemon.kill('SIGKILL');
    }
  });
});
