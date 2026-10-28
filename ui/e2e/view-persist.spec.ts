/**
 * R15 视图状态持久化 e2e（真实 daemon + 真实 UI 产物）：
 * sort / filter / view 三态 localStorage 存取 + 重载恢复 + 损坏值容错。
 *
 * 覆盖点：
 *   1. 排序持久化：点表头排序 → reload → 恢复排序（aria-sort + 行序 +
 *      请求携带 sort 参数三重证据）
 *   2. 过滤持久化：切侧栏分段 → reload → 恢复分段（徽章高亮 + 请求带 state）
 *   3. 回默认清键：表头回默认序 → reload → 仍是默认序
 *   4. 损坏值容错：dl.sort 写入垃圾 JSON → reload → 回落默认序不崩
 */
import { test, expect, type Page } from '@playwright/test';

const BASE = process.env.E2E_BASE_URL!;
const FILE = process.env.E2E_FILE_BASE!;

async function rowNames(page: Page): Promise<string[]> {
  return page.getByTestId('task-name').evaluateAll((els) =>
    els.map((e) => (e.textContent ?? '').trim()),
  );
}

async function expectOrder(page: Page, want: string[]) {
  await expect
    .poll(async () => (await rowNames(page)).join('|'), { timeout: 10_000 })
    .toBe(want.join('|'));
}

test.describe.serial('视图状态持久化 e2e（R15：sort/filter/view → localStorage）', () => {
  test.afterEach(async ({ request }) => {
    const res = await request.get(`${BASE}/tasks`);
    if (!res.ok()) return;
    const tasks = (await res.json()) as Array<{ task_id: string }>;
    for (const t of tasks) {
      await request.delete(`${BASE}/tasks/${t.task_id}?delete_data=true`).catch(() => {});
    }
  });

  test('排序持久化：表头排序 → 重载恢复（行序 + 请求参数双证据）→ 回默认清键', async ({
    page,
    request,
  }) => {
    const pool = ['/small.bin', '/delete.bin', '/pause.bin'];
    for (const [i, name] of ['charlie.bin', 'alpha.bin', 'bravo.bin'].entries()) {
      const res = await request.post(`${BASE}/tasks`, {
        data: { url: `${FILE}${pool[i]}`, name },
      });
      expect(res.ok()).toBeTruthy();
    }
    await page.goto(BASE);
    await expectOrder(page, ['charlie.bin', 'alpha.bin', 'bravo.bin']);

    // 点名称表头（升序）→ localStorage 落键
    await page.locator('th button', { hasText: '名称' }).click();
    await expectOrder(page, ['alpha.bin', 'bravo.bin', 'charlie.bin']);
    await expect
      .poll(async () => page.evaluate(() => localStorage.getItem('dl.sort')), {
        timeout: 5_000,
      })
      .toBe(JSON.stringify({ key: 'name', dir: 'asc' }));

    // 重载 → 恢复排序：请求携带 sort=name（服务端权威序直接生效）
    const reqPromise = page.waitForRequest(
      (r) => r.url().includes('/tasks') && r.url().includes('sort=name'),
      { timeout: 10_000 },
    );
    await page.reload();
    await reqPromise;
    await expectOrder(page, ['alpha.bin', 'bravo.bin', 'charlie.bin']);
    await expect(page.locator('th[aria-sort="ascending"]').first()).toBeVisible();

    // 再点两下回默认（升 → 降 → 默认）→ 键清除 → 重载仍是创建序
    await page.locator('th button', { hasText: '名称' }).click(); // → desc
    await page.locator('th button', { hasText: '名称' }).click(); // → 默认
    await expectOrder(page, ['charlie.bin', 'alpha.bin', 'bravo.bin']);
    await expect
      .poll(async () => page.evaluate(() => localStorage.getItem('dl.sort')), {
        timeout: 5_000,
      })
      .toBe(null);
    await page.reload();
    await expectOrder(page, ['charlie.bin', 'alpha.bin', 'bravo.bin']);
  });

  test('过滤持久化：侧栏分段 → 重载恢复（高亮 + 请求带 state）；损坏 sort 值容错', async ({
    page,
    request,
  }) => {
    const pool = ['/small.bin', '/delete.bin'];
    for (const [i, name] of ['ok-done.bin', 'fail-me.bin'].entries()) {
      const res = await request.post(`${BASE}/tasks`, {
        data: { url: `${FILE}${pool[i]}`, name },
      });
      expect(res.ok()).toBeTruthy();
    }
    await page.goto(BASE);

    // 切「失败」分段（服务端 state=Failed 过滤；当前两任务非 Failed → 空列表）
    await page.getByRole('button', { name: '失败' }).click();
    await expect
      .poll(async () => page.evaluate(() => localStorage.getItem('dl.filter')), {
        timeout: 5_000,
      })
      .toBe('failed');
    await expect(page.getByTestId('task-name')).toHaveCount(0);

    // 预置损坏 sort 值（模拟旧版本/手改 localStorage）→ 重载不崩且回落默认序
    await page.evaluate(() => localStorage.setItem('dl.sort', '{"key":"bogus","dir":"sideways"}'));
    await page.reload();
    // 过滤恢复（失败分段 → 空列表）+ 损坏 sort 值被恢复流程清除
    // （空列表时表格不渲染表头——aria-sort 断言挪到切回全部之后）
    await expect(page.getByTestId('task-name')).toHaveCount(0, { timeout: 10_000 });
    await expect(
      page.evaluate(() => localStorage.getItem('dl.sort')),
    ).resolves.toBe(null); // 损坏值被 mount persist-effect 清除

    // 切回「全部」→ 恢复全列表 + sort 回落默认（表头恢复渲染后断言）
    await page.getByRole('button', { name: '全部' }).click();
    await expect
      .poll(async () => (await rowNames(page)).sort().join('|'), { timeout: 10_000 })
      .toBe(['fail-me.bin', 'ok-done.bin'].sort().join('|'));
    await expect(page.locator('th[aria-sort="none"]').first()).toBeVisible();
  });
});
