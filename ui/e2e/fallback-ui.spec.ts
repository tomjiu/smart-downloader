/**
 * R21 云兜底 UI 接入 e2e（体检 #182 第 4 真缺口收口）：
 * 按钮可见性守卫（engine门控）+ 非 BT 任务不渲染。
 *
 * 正向路径（BT+Paused+Provider 兜底完成）的端到端验证归 #191 bt-panel
 * 套件（需 bt feature daemon + 本地 swarm，主套件为非 bt 构建）；
 * daemon 侧行为契约已由 fallback_api.rs 3 条锁定（直链完成/无 Provider
 * 409/禁用跳过）。本 spec 锁 UI 门控与不误渲染。
 */
import { test, expect } from '@playwright/test';

const BASE = process.env.E2E_BASE_URL!;
const FILE = process.env.E2E_FILE_BASE!;

test.describe.serial('云兜底 UI 门控（R21：#182 第 4 真缺口）', () => {
  test.afterEach(async ({ request }) => {
    const res = await request.get(`${BASE}/tasks`);
    if (!res.ok()) return;
    const tasks = (await res.json()) as Array<{ task_id: string }>;
    for (const t of tasks) {
      await request.delete(`${BASE}/tasks/${t.task_id}?delete_data=true`).catch(() => {});
    }
  });

  test('非 BT 任务（HTTP 暂停态）不渲染兜底按钮（engine 门控）', async ({
    page,
    request,
  }) => {
    // HTTP 任务 → 暂停 → 开详情：engine !== bt → 按钮不渲染
    const add = await request.post(`${BASE}/tasks`, {
      data: { url: `${FILE}/pause.bin`, name: 'fallback-guard.bin' },
    });
    expect(add.ok()).toBeTruthy();
    const id = (await add.json()).task_id as string;
    await request.post(`${BASE}/tasks/${id}/pause`);

    await page.goto(BASE);
    const row = page.getByTestId('task-row').first();
    await expect(row).toBeVisible({ timeout: 10_000 });
    await row.dblclick();
    await expect(page.getByText('任务级控制')).toBeVisible({ timeout: 5_000 });
    // engine 门控：HTTP 任务无论何种状态都不出现兜底按钮
    await expect(page.getByTestId('fallback-btn')).toHaveCount(0);
  });
});
