/**
 * 任务级操作日志 e2e（体检 #182 P2 补全：/tasks/:id/logs 端点接入 UI）。
 * 契约：详情抽屉「操作日志」区渲染任务的审计轨迹（add/pause/resume/
 * remove/restored + detail），区别于日志视图的全局事件流。
 */
import { test, expect } from '@playwright/test';
const BASE = process.env.E2E_BASE_URL!;
const FILE = process.env.E2E_FILE_BASE!;

test.describe.serial('任务详情操作日志', () => {
  // 清场：删光任务（含数据）——串行共享 daemon 下跨套件零残留
  // （超合并验证 F4：本套件曾是无 afterEach 的唯一建任务者，pause.bin
  // 残留毒害后续 view-persist 的行序断言）
  test.afterEach(async ({ request }) => {
    const res = await request.get(`${BASE}/tasks`);
    if (!res.ok()) return;
    for (const t of (await res.json()) as Array<{ task_id: string }>) {
      await request.delete(`${BASE}/tasks/${t.task_id}?delete_data=true`).catch(() => {});
    }
  });

  test('任务详情操作日志（add + pause 渲染 + API 对齐）', async ({ page, request }) => {
    await page.goto(BASE!);
    await page.getByTestId('new-task-btn').click();
    await page.getByTestId('add-url-input').fill(`${FILE}/pause.bin?lg`);
    await page.getByTestId('add-submit').click();
    await expect(page.getByTestId('add-url-input')).toBeHidden({ timeout: 10_000 });
    await expect(page.getByTestId('task-row')).toHaveCount(1, { timeout: 15_000 });
    // 等 Downloading 再暂停（Queued/Evaluating 期按钮语义是「强制开始」——qB 式设计）
    await expect(page.getByTestId('task-state').first()).toHaveText(/下载|Downloading/, {
      timeout: 15_000,
    });
    await page.getByTestId('pause-resume-btn').first().click();
    await page.waitForTimeout(800);
    // 打开详情 → 操作日志区出现 add + pause
    await page.getByTestId('task-row').first().dblclick();
    await expect(page.getByLabel('任务详情')).toBeVisible();
    await expect(page.getByLabel('任务操作日志')).toBeVisible({ timeout: 10_000 });
    const ops = await page.getByTestId('task-log-op').allTextContents();
    expect(ops).toContain('add');
    expect(ops).toContain('pause');
    // API 对齐：UI 展示的事件集与 GET /tasks/:id/logs 一致
    const tasks = (await (await request.get(`${BASE}/tasks`)).json()) as Array<{
      task_id: string;
    }>;
    const api = (await (await request.get(`${BASE}/tasks/${tasks[0].task_id}/logs`)).json()) as {
      events: Array<{ op: string }>;
    };
    expect(ops).toEqual(api.events.map((e) => e.op));
  });
});
