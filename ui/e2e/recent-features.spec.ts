/**
 * #173 / #164 近期合并功能的 UI 端到端（真实 daemon + 真实下载 + 真实 UI 产物）。
 *
 * 覆盖（关键路径套件之外的新增面）：
 *   1. #173 服务端列表过滤：侧栏分段（进行中/已完成/已暂停）点击后仅渲染
 *      匹配行 + 搜索框服务端 ?search=（防抖后）过滤 + 分段徽章计数来自
 *      /stats 聚合（列表已被过滤，不能再当计数源——#173 的核心契约）
 *   2. #164 全局限速总阀门（统计页卡片：pill 回显 + API 口径一致）+
 *      Provider 状态可见（默认未注册空态）
 *
 * 造数走 API（curl 语义）：小文件秒级完成、pause.bin 中途暂停——比 UI
 * 逐个添加更快且不依赖新建表单交互（表单路径已由关键路径 #1/#7 覆盖）。
 *
 * 虚拟滚动契约（同 critical-paths）：行级断言前 scrollIntoViewIfNeeded；
 * 「列表为空」用 toHaveCount(0)。
 */
import { test, expect, type Page } from '@playwright/test';

const BASE = process.env.E2E_BASE_URL!;
const FILE = process.env.E2E_FILE_BASE!;

/** 等 UI 列表出现指定行数（轮询 500ms，上限 ms）。 */
async function waitRows(page: Page, n: number, timeoutMs = 10_000) {
  await expect(page.getByTestId('task-row')).toHaveCount(n, { timeout: timeoutMs });
}

/** API 添加任务（跳过 UI 表单——表单路径已由关键路径覆盖）。 */
async function addViaApi(page: Page, url: string): Promise<string> {
  const r = await page.request.post(`${BASE}/tasks`, { data: { url } });
  expect(r.ok(), `add ${url}`).toBeTruthy();
  return (await r.json()).task_id;
}

/** 等 API 侧任务到达目标态。 */
async function waitState(page: Page, id: string, state: string, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const r = await page.request.get(`${BASE}/tasks/${id}`);
    if (r.ok() && (await r.json()).state === state) return;
    await page.waitForTimeout(400);
  }
  throw new Error(`task ${id} 未在 ${timeoutMs}ms 内到达 ${state}`);
}

test.describe.serial('近期功能 e2e（#173 过滤 + #164 全局限速/Provider）', () => {
  test.afterEach(async ({ request }) => {
    // 清场：删光任务 + 全局限速复位（0 = 不限），保住下一条用例干净起点
    const res = await request.get(`${BASE}/tasks`);
    if (res.ok()) {
      for (const t of await res.json()) {
        await request.delete(`${BASE}/tasks/${t.task_id}?purge=true`);
      }
    }
    await request.post(`${BASE}/config/limit`, { data: { max_download_kb_s: 0 } });
  });

  test('1. #173 服务端分段过滤：仅渲染匹配行 + 徽章计数走 /stats 聚合', async ({ page }) => {
    await page.goto('/');
    // 造数：small 秒级完成；pause.bin 中途暂停（API 暂停）
    const tDone = await addViaApi(page, `${FILE}/small.bin`);
    const tPaused = await addViaApi(page, `${FILE}/pause.bin`);
    await waitState(page, tDone, 'Completed');
    await page.waitForTimeout(2_500); // 给 pause.bin 起步进度
    await page.request.post(`${BASE}/tasks/${tPaused}/pause`);
    await waitState(page, tPaused, 'Paused');
    // UI 落定：两行都渲染（回到「全部」段）
    await page.getByRole('button', { name: '全部' }).click();
    await waitRows(page, 2);

    // 「已完成」段：只渲染 Completed 行（Stopped/Seeding 也属该段，此处仅 Completed）
    await page.getByRole('button', { name: '已完成' }).click();
    await waitRows(page, 1);
    await expect(page.getByTestId('task-row').first()).toHaveAttribute('data-task-state', 'Completed');

    // 「已暂停」段：只渲染 Paused 行
    await page.getByRole('button', { name: '已暂停' }).click();
    await waitRows(page, 1);
    await expect(page.getByTestId('task-row').first()).toHaveAttribute('data-task-state', 'Paused');

    // 「进行中」段：无匹配（两个任务都不在传）——空态渲染 0 行
    await page.getByRole('button', { name: '进行中' }).click();
    await waitRows(page, 0);

    // 回「全部」→ 2 行恢复（过滤可逆，不丢任务）
    await page.getByRole('button', { name: '全部' }).click();
    await waitRows(page, 2);

    // 分段徽章计数：与 /stats by_state 聚合一致（当前 Completed=1 Paused=1）
    const badgeDone = page.getByRole('button', { name: '已完成' }).locator('.dl-side-count');
    await expect(badgeDone).toHaveText('1');
    const badgePaused = page.getByRole('button', { name: '已暂停' }).locator('.dl-side-count');
    await expect(badgePaused).toHaveText('1');
  });

  test('2. #173 服务端搜索：防抖后 ?search= 生效（匹配/清空恢复）', async ({ page }) => {
    await page.goto('/');
    await addViaApi(page, `${FILE}/small.bin`);
    await addViaApi(page, `${FILE}/delete.bin`);
    await waitRows(page, 2);

    // 搜索命中一名：small.bin（delete.bin 8MB@512KiB/s 仍在下，行也在）
    await page.getByTestId('search-input').fill('small');
    await waitRows(page, 1);
    await expect(page.getByTestId('task-name').first()).toContainText('small.bin');

    // 大小写不敏感（daemon 侧口径）
    await page.getByTestId('search-input').fill('SMALL');
    await waitRows(page, 1);

    // 无匹配 → 空态 0 行（虚拟滚动契约：toHaveCount(0)）
    await page.getByTestId('search-input').fill('nomatch-xyz');
    await waitRows(page, 0);

    // 清空搜索（clear 按钮）→ 全量恢复
    await page.locator('.dl-search-clear').click();
    await waitRows(page, 2);
  });

  test('3. #164 统计页：全局限速总阀门 pill 回显 + API 口径一致 + Provider 空态', async ({ page }) => {
    await page.goto('/');
    await page.getByRole('button', { name: '统计' }).click();

    // Provider 卡片：默认配置未注册 → 空态文案
    await expect(page.getByText('未注册 Provider')).toBeVisible({ timeout: 10_000 });

    // 全局速率卡片：初始「不限」（作用域限本卡——状态栏另有同名 pill）
    const limitCard = page.locator('.dl-card', { hasText: '全局速率限制' });
    const downPill = limitCard.locator('.dl-pill--down');
    await expect(downPill).toContainText('不限', { timeout: 10_000 });

    // 设 64 KiB/s → 立即生效 → pill 回显 64 KiB/s
    await page.getByPlaceholder('下行 KiB/s（0=不限）').fill('64');
    await page.getByRole('button', { name: '立即生效' }).click();
    await expect(downPill).toContainText('64 KiB/s', { timeout: 10_000 });

    // API 口径一致（#164 的验收锚点：UI 与 /config 同源）
    const r = await page.request.get(`${BASE}/config`);
    expect(r.ok()).toBeTruthy();
    expect((await r.json()).max_download_kb_s).toBe(64);

    // 复位 0 → 「不限」回显（afterEach 亦兜底复位）
    await page.getByPlaceholder('下行 KiB/s（0=不限）').fill('0');
    await page.getByRole('button', { name: '立即生效' }).click();
    await expect(downPill).toContainText('不限', { timeout: 10_000 });
  });
});
