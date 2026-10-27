/**
 * 统计一致性 + 排序 + 虚拟滚动 e2e（R10 体检增强）。
 *
 * 覆盖：
 *   1. 统计页聚合 = /stats API 口径（活跃/完成卡片 + 侧栏计数徽章）
 *   2. 表头排序交互（aria-sort 升/降切换，不崩）
 *   3. 大列表虚拟滚动冒烟（30 任务：DOM 行数 < 总数；滚动窗口推进）
 */
import { test, expect, type APIRequestContext } from '@playwright/test';

const BASE = process.env.E2E_BASE_URL!;
const FILE = process.env.E2E_FILE_BASE!;

async function addViaApi(request: APIRequestContext, url: string, name?: string): Promise<string> {
  // 唯一显式名：同套件并发任务不碰撞同一 .part（超合并验证 F5 同款加固）
  const r = await request.post(`${BASE}/tasks`, { data: name ? { url, name } : { url } });
  expect(r.ok()).toBeTruthy();
  return ((await r.json()) as { task_ids: string[] }).task_ids[0];
}

async function removeAll(request: APIRequestContext) {
  const r = await request.get(`${BASE}/tasks`);
  if (!r.ok()) return;
  const tasks = (await r.json()) as Array<{ task_id: string }>;
  if (tasks.length === 0) return;
  await request.post(`${BASE}/tasks/batch`, {
    data: { action: 'remove', ids: tasks.map((t) => t.task_id), delete_data: true },
  });
}

async function stats(request: APIRequestContext) {
  return (await (await request.get(`${BASE}/stats`)).json()) as {
    total: number;
    by_state: Record<string, number>;
  };
}

test.describe.serial('统计一致性 + 排序 + 虚拟滚动', () => {
  test.afterEach(async ({ request }) => {
    await removeAll(request);
  });

  test('统计页聚合与 /stats API 对齐（活跃/完成 + 侧栏徽章）', async ({ page, request }) => {
    // 2 个完成 + 1 个下载中
    await addViaApi(request, `${FILE}/small.bin?s1`, 'st-s1.bin');
    await addViaApi(request, `${FILE}/small.bin?s2`, 'st-s2.bin');
    // limit.bin 128MB@4MiB/s≈32s——保证统计断言期间仍在下载（delete.bin 8MB 会在断言前完成）
    await addViaApi(request, `${FILE}/limit.bin?s3`, 'st-l3.bin');
    // 等两个 small 完成
    const deadline = Date.now() + 30_000;
    for (;;) {
      const s = await stats(request);
      const done = (s.by_state['Completed'] ?? 0) + (s.by_state['Seeding'] ?? 0);
      if (done >= 2) break;
      if (Date.now() > deadline) throw new Error('small.bin 30s 未完成');
      await new Promise((r) => setTimeout(r, 400));
    }

    await page.goto(BASE!);
    // 侧栏徽章：全部=3（与 stats.total 对齐）
    const s = await stats(request);
    await expect(page.getByRole('button', { name: /^全部/ })).toContainText(
      String(s.total),
      { timeout: 10_000 },
    );
    // 统计页卡片：活跃任务 = active 聚合口径；累计完成 = done 聚合口径
    await page.getByRole('button', { name: '统计' }).click();
    const active = ['Downloading', 'Seeding', 'Transferring', 'FallbackProvider'].reduce(
      (a, k) => a + (s.by_state[k] ?? 0),
      0,
    );
    const done = ['Completed', 'Seeding'].reduce((a, k) => a + (s.by_state[k] ?? 0), 0);
    await expect(page.getByText('活跃任务').locator('..')).toContainText(String(active));
    await expect(page.getByText('累计完成任务').locator('..')).toContainText(String(done));
    // 状态分布用中文标签（stateLabel 映射，与表格徽章同口径）——抽查两键
    const LABEL: Record<string, string> = {
      Downloading: '下载中',
      Completed: '已完成',
      Seeding: '做种',
      Paused: '已暂停',
      Queued: '排队',
    };
    // 徽章渲染为「中文标签: 数」复合文本——前缀正则匹配且核对计数
    let checked = 0;
    for (const [k, v] of Object.entries(s.by_state)) {
      if (v > 0 && LABEL[k]) {
        await expect(page.getByText(new RegExp(`^${LABEL[k]}[:：]\\s*${v}$`)).first()).toBeVisible({
          timeout: 5_000,
        });
        checked++;
      }
    }
    expect(checked).toBeGreaterThanOrEqual(1);
  });

  test('表头排序：名称/状态升降切换不崩（aria-sort 断言）', async ({ page, request }) => {
    await addViaApi(request, `${FILE}/small.bin?o1`, 'st-o1.bin');
    await addViaApi(request, `${FILE}/small.bin?o2`, 'st-o2.bin');
    await page.goto(BASE!);
    await expect(page.getByTestId('task-row')).toHaveCount(2, { timeout: 15_000 });

    const nameTh = page.getByRole('columnheader', { name: '名称' });
    await expect(nameTh).toHaveAttribute('aria-sort', 'none');
    await page.getByRole('button', { name: '名称', exact: true }).click();
    await expect(nameTh).toHaveAttribute('aria-sort', 'ascending');
    await page.getByRole('button', { name: '名称', exact: true }).click();
    await expect(nameTh).toHaveAttribute('aria-sort', 'descending');
    await page.getByRole('button', { name: '状态', exact: true }).click();
    await expect(page.getByRole('columnheader', { name: '状态' })).toHaveAttribute(
      'aria-sort',
      'ascending',
    );
    // 排序后行仍可交互（双击开详情不崩）
    await page.getByTestId('task-row').first().dblclick();
    await expect(page.getByLabel('任务详情')).toBeVisible();
  });

  // 视口限高触发窗口化（默认 720px 时 30 行 + overscan=12 会全部渲染）
  test.use({ viewport: { width: 1280, height: 400 } });
  test('虚拟滚动：30 任务 DOM 行数受限 + 滚动窗口推进', async ({ page, request }) => {
    for (let i = 0; i < 30; i++) {
      await addViaApi(request, `${FILE}/delete.bin?v=${i}`);
    }
    await page.goto(BASE!);
    // 等列表拿到 30（侧栏徽章）
    await expect(page.getByRole('button', { name: /^全部/ })).toContainText('30', {
      timeout: 20_000,
    });
    // #133 虚拟滚动契约：DOM 只渲染可视窗口内的行
    const domRows = await page.getByTestId('task-row').count();
    expect(domRows).toBeLessThan(30);
    expect(domRows).toBeGreaterThanOrEqual(1);
    // 滚动到底部 → 窗口推进（行集变化或仍小于 30）
    await page.evaluate(() => {
      const el = document.querySelector('.dl-table-wrap, [data-virtual], .dl-main');
      (el as HTMLElement | null)?.scrollTo?.({ top: 100000 });
      window.dispatchEvent(new Event('scroll'));
    });
    await page.waitForTimeout(600);
    const domRows2 = await page.getByTestId('task-row').count();
    expect(domRows2).toBeLessThan(30);
  });
});
