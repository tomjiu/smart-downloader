/**
 * R14 服务端排序 UI e2e（真实 daemon + 真实 UI 产物）：
 * 表头点击 → 请求携带 ?sort=&order=（服务端排序唯一权威）→ 行序回程断言。
 *
 * 覆盖点：
 *   1. 名称表头三态循环：默认（创建序）→ 升 → 降 → 默认
 *   2. 网络层断言：列表请求确实携带 sort/order 参数（UI↔daemon 对齐证据）
 *   3. DOM 行序 = 服务端返回序（客户端不再二次排序）
 *
 * 虚拟滚动契约：3 任务场景全行在窗口内，DOM 序即可断言。
 * aria-sort 属性断言与 #192 stats-sort.spec.ts 分工（本文件断行序，
 * 那边断属性循环 + 统计一致性 + 虚拟滚动）。
 */
import { test, expect, type Page } from '@playwright/test';

const BASE = process.env.E2E_BASE_URL!;
const FILE = process.env.E2E_FILE_BASE!;

/** 按创建序添加带显式名的任务（走 API——本套件关注排序回程，不走新建表单）。 */
async function addNamed(request: any, url: string, name: string) {
  const res = await request.post(`${BASE}/tasks`, {
    data: { url, name },
  });
  expect(res.ok()).toBeTruthy();
}

/** 当前 DOM 行序的任务名列表（task-name 按文档序返回 = 显示序）。 */
async function rowNames(page: Page): Promise<string[]> {
  return page.getByTestId('task-name').evaluateAll((els) =>
    els.map((e) => (e.textContent ?? '').trim()),
  );
}

/** 等待行序达到期望（轮询 1s 内重拉；超时由 expect 兜底）。 */
async function expectOrder(page: Page, want: string[]) {
  await expect
    .poll(async () => (await rowNames(page)).join('|'), { timeout: 10_000 })
    .toBe(want.join('|'));
}

test.describe.serial('服务端排序 e2e（R14：表头 → ?sort=&order= → 行序回程）', () => {
  test.afterEach(async ({ request }) => {
    const res = await request.get(`${BASE}/tasks`);
    if (!res.ok()) return;
    const tasks = (await res.json()) as Array<{ task_id: string }>;
    for (const t of tasks) {
      await request.delete(`${BASE}/tasks/${t.task_id}?delete_data=true`).catch(() => {});
    }
  });

  test('名称表头三态：默认序 → 升序（请求带 sort=name&order=asc）→ 降序 → 回默认', async ({
    page,
    request,
  }) => {
    // 造 3 任务：创建序 charlie → alpha → bravo（名称乱序，判别力明确；
    // URL 取 file-server 既有四个文件——同 URL 会被 canonical 去重 409）
    const pool1 = ['/small.bin', '/delete.bin', '/pause.bin'];
    for (const [i, name] of ['charlie.bin', 'alpha.bin', 'bravo.bin'].entries()) {
      await addNamed(request, `${FILE}${pool1[i]}`, name);
    }
    await page.goto(BASE);

    // 默认序 = 创建序（服务端无 sort 参数）
    await expectOrder(page, ['charlie.bin', 'alpha.bin', 'bravo.bin']);

    // 网络层：先挂请求监听再点名称表头（防竞态）→ 列表请求携带
    // sort=name&order=asc（UI→daemon 排序下推的直接证据）
    const reqPromise = page.waitForRequest(
      (r) => r.url().includes('/tasks') && r.url().includes('sort=name'),
      { timeout: 10_000 },
    );
    await page.locator('th button', { hasText: '名称' }).click();
    const sortUrl = (await reqPromise).url();
    expect(sortUrl).toContain('order=asc');
    // th 的 aria-sort 联动（属性断言归 #192，此处仅冒烟一档）
    await expect(
      page.locator('th[aria-sort="ascending"]').first(),
    ).toBeVisible();

    // 升序行序（服务端返回序 = 显示序）
    await expectOrder(page, ['alpha.bin', 'bravo.bin', 'charlie.bin']);

    // 再点 → 降序（先挂监听防竞态）
    const descReqPromise = page.waitForRequest(
      (r) => r.url().includes('/tasks') && r.url().includes('order=desc'),
      { timeout: 10_000 },
    );
    await page.locator('th button', { hasText: '名称' }).click();
    await descReqPromise;
    await expectOrder(page, ['charlie.bin', 'bravo.bin', 'alpha.bin']);
    await expect(page.locator('th[aria-sort="descending"]').first()).toBeVisible();

    // 第三点 → 回默认（无 sort 参数，创建序）
    await page.locator('th button', { hasText: '名称' }).click();
    await expectOrder(page, ['charlie.bin', 'alpha.bin', 'bravo.bin']);
    await expect(page.locator('th[aria-sort="none"]').first()).toBeVisible();
  });

  test('排序与过滤共存：搜索过滤 + 名称排序同时生效（服务端组合语义）', async ({
    page,
    request,
  }) => {
    const pool2 = ['/small.bin', '/delete.bin', '/pause.bin', '/limit.bin'];
    for (const [i, name] of [
      'zeta.bin',
      'mid-beta.bin',
      'mid-alpha.bin',
      'other.bin',
    ].entries()) {
      await addNamed(request, `${FILE}${pool2[i]}`, name);
    }
    await page.goto(BASE);

    // 搜索 "mid" → 2 任务（创建序 mid-beta, mid-alpha）
    await page.getByTestId('search-input').fill('mid');
    await expectOrder(page, ['mid-beta.bin', 'mid-alpha.bin']);

    // 点名称表头 → 过滤 + 排序组合（服务端 ?search=mid&sort=name&order=asc）
    await page.locator('th button', { hasText: '名称' }).click();
    await expectOrder(page, ['mid-alpha.bin', 'mid-beta.bin']);

    // API 侧同参对照（三方一致：UI 显示序 = API 直查序）
    const api = await request.get(`${BASE}/tasks?search=mid&sort=name&order=asc`);
    expect(api.ok()).toBeTruthy();
    const rows = (await api.json()) as Array<{ name?: string }>;
    expect(rows.map((r) => r.name)).toEqual(['mid-alpha.bin', 'mid-beta.bin']);
  });
});
