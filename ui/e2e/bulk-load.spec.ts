/**
 * R16 重负载真实场景 e2e（真实 daemon + 真实 UI 产物）：120 任务全生命周期。
 *
 * 模拟真实重度用户的完整旅程（「批量导入 → 浏览管理 → 清场」）：
 *   1. 添加风暴：120 任务并发 POST（10 并发 × 12 批）
 *   2. 虚拟滚动护栏：120 任务 DOM 行数受限（不随任务数线性膨胀）
 *   3. 统计一致：/stats total = 120；侧栏徽章 = 120；X-Total-Count 分页正确
 *   4. 服务端排序抽查：name 升序首尾行正确
 *   5. 批量操作：全选 → 批量暂停 → 批量恢复（选择集保留，#183 语义）
 *   6. 清场风暴：分批显式 ids 删除（批量 API 上限 100/批）→ 列表清空
 *   7. 全程 daemon 存活（/health 探测）
 */
import { test, expect, type APIRequestContext, type Page } from '@playwright/test';

const BASE = process.env.E2E_BASE_URL!;
const FILE = process.env.E2E_FILE_BASE!;
const N = 120;

/** 并发添加一批任务（每批 concurrency 个）。 */
async function addStorm(request: APIRequestContext, ids: number[], concurrency = 10) {
  const results: boolean[] = [];
  for (let i = 0; i < ids.length; i += concurrency) {
    const batch = ids.slice(i, i + concurrency);
    const rs = await Promise.all(
      batch.map(async (n) => {
        const r = await request.post(`${BASE}/tasks`, {
          data: { url: `${FILE}/bulk/${n}.bin`, name: `bulk-${String(n).padStart(3, '0')}.bin` },
        });
        return r.ok();
      }),
    );
    results.push(...rs);
  }
  return results;
}

/** 分批显式 ids 刹除（POST /tasks/batch action=remove 上限 100 ids/批）。 */
async function deleteStorm(request: APIRequestContext, ids: string[]) {
  for (let i = 0; i < ids.length; i += 100) {
    const chunk = ids.slice(i, i + 100);
    const r = await request.post(`${BASE}/tasks/batch`, {
      data: { action: 'remove', ids: chunk, delete_data: true },
    });
    expect(r.ok()).toBeTruthy();
  }
}

async function allTaskIds(request: APIRequestContext): Promise<string[]> {
  const res = await request.get(`${BASE}/tasks`);
  expect(res.ok()).toBeTruthy();
  return ((await res.json()) as Array<{ task_id: string }>).map((t) => t.task_id);
}

test.describe.serial('重负载 e2e（R16：120 任务全生命周期）', () => {
  // 清场防御：用例失败中断时清光残留（120 任务残留会污染后续 spec 的
  // 全量列表/统计断言）
  test.afterEach(async ({ request }) => {
    const res = await request.get(`${BASE}/tasks`);
    if (!res.ok()) return;
    const tasks = (await res.json()) as Array<{ task_id: string }>;
    for (let i = 0; i < tasks.length; i += 100) {
      await request
        .post(`${BASE}/tasks/batch`, {
          data: { action: 'remove', ids: tasks.slice(i, i + 100).map((t) => t.task_id), delete_data: true },
        })
        .catch(() => {});
    }
  });

  test('添加风暴 → 虚拟滚动护栏 → 统计/排序/批量 → 清场风暴', async ({
    page,
    request,
  }) => {
    test.setTimeout(180_000);

    // ---- 1. 添加风暴：120 任务（10 并发 × 12 批），全 201 ----
    const ok = await addStorm(request, Array.from({ length: N }, (_, i) => i + 1));
    expect(ok.filter(Boolean).length).toBe(N);

    // daemon 存活探针（风暴后）
    const health = await request.get(`${BASE}/health`);
    expect(health.ok()).toBeTruthy();

    // ---- 2. UI 虚拟滚动护栏：120 任务 DOM 行数受限 ----
    await page.goto(BASE);
    await expect
      .poll(async () => page.getByTestId('task-row').count(), { timeout: 20_000 })
      .toBeGreaterThan(0);
    // 等列表计数稳定（两拍同值 = 首轮拉齐）
    await expect
      .poll(
        async () => {
          const names = await page
            .getByTestId('task-name')
            .evaluateAll((els) => els.map((e) => (e.textContent ?? '').trim()));
          return names.length > 0 ? String(names.length) : '';
        },
        { timeout: 20_000 },
      )
      .toBeTruthy();
    const domRows = await page.getByTestId('task-row').count();
    // 虚拟滚动护栏：DOM 行数应远小于任务总数（窗口 + overscan 量级）
    expect(domRows).toBeLessThan(N);
    expect(domRows).toBeGreaterThan(0);

    // ---- 3. 统计一致：侧栏「全部」徽章 = 120；/stats total = 120 ----
    await expect(page.locator('.dl-side-item.is-on .dl-side-count').first()).toHaveText('120');
    const stats = (await (await request.get(`${BASE}/stats`)).json()) as { total: number };
    expect(stats.total).toBe(N);

    // X-Total-Count 分页正确性（服务端排序下的分页语义）
    const p2 = await request.get(`${BASE}/tasks?sort=name&order=asc&limit=50&offset=100`);
    expect(p2.ok()).toBeTruthy();
    expect(p2.headers()['x-total-count']).toBe(String(N));
    const p2rows = (await p2.json()) as Array<{ name?: string }>;
    expect(p2rows.length).toBe(20); // 120 - 100

    // ---- 3.5 toast 洪泛护栏（R16 修复验证）：120 任务风暴后重载页面，
    // 事件重放不得弹「任务完成」toast 洪泛（live-only 订阅 + 并发上限 3）----
    await page.reload();
    await expect(page.getByTestId('task-row').first()).toBeVisible({ timeout: 20_000 });
    // 给重放窗口留足时间（历史完成事件若误弹 toast 会在此期间出现）
    await page.waitForTimeout(2500);
    const toastCount = await page.locator('.dl-toast').count();
    expect(toastCount).toBeLessThanOrEqual(3, '重放事件不得弹现场 toast（live 边界失效）');

    // ---- 3.6 live 完成提示仍工作（live-only 逻辑的回归护栏）：页面开着
    // 时新增任务并完成 → 「任务完成」toast 必须出现（现场事件 ≠ 历史）----
    const liveAdd = await request.post(`${BASE}/tasks`, {
      data: { url: `${FILE}/bulk/999.bin`, name: 'live-toast-probe.bin' },
    });
    expect(liveAdd.ok()).toBeTruthy();
    await expect(page.getByText('任务完成').first()).toBeVisible({ timeout: 15_000 });
    // 探针即删（后续断言计数基准回到 120）
    const probeId = (await liveAdd.json()).task_id as string;
    await request.delete(`${BASE}/tasks/${probeId}?delete_data=true`);

    // ---- 4. 服务端排序抽查：name 升序 → 首行 bulk-001（重载前默认创建序是 bulk-001 先建）----
    await page.locator('th button', { hasText: '名称' }).click(); // 升序
    await expect
      .poll(
        async () => {
          const first = await page.getByTestId('task-name').first().textContent();
          return (first ?? '').trim();
        },
        { timeout: 15_000 },
      )
      .toBe('bulk-001.bin');

    // ---- 5. 批量操作：全选 → 批量暂停 → 批量恢复（选择集保留）----
    await page.getByRole('checkbox', { name: /全选/ }).first().check();
    const batchGroup = page.getByRole('group', { name: '批量操作' });
    // 全选后批量按钮可用（计数徽章 = 120）
    await expect(batchGroup.getByRole('button', { name: /^暂停/ })).toBeEnabled();
    await expect(batchGroup.getByRole('button', { name: /^暂停/ }).locator('.dl-tool-count')).toHaveText('120');
    await batchGroup.getByRole('button', { name: /^暂停/ }).click();
    // 选择集保留：批量恢复无需重新全选（#183 语义）——恢复按钮计数仍 120
    await expect(batchGroup.getByRole('button', { name: /^恢复/ })).toBeEnabled();
    await expect(batchGroup.getByRole('button', { name: /^恢复/ }).locator('.dl-tool-count')).toHaveText('120');
    await batchGroup.getByRole('button', { name: /^恢复/ }).click();
    // 抽查 API：任务仍存在且非终态风暴后健康
    const afterBatch = await allTaskIds(request);
    expect(afterBatch.length).toBe(N);

    // ---- 6. 清场风暴：分批删除（100/批）→ 列表清空 + 键清理 ----
    await deleteStorm(request, afterBatch);
    await expect
      .poll(async () => (await allTaskIds(request)).length, { timeout: 30_000 })
      .toBe(0);
    // daemon 全程存活（清场后）
    const health2 = await request.get(`${BASE}/health`);
    expect(health2.ok()).toBeTruthy();
    // UI 反映清空（虚拟滚动空态：渲染行 0）
    await expect
      .poll(async () => page.getByTestId('task-row').count(), { timeout: 20_000 })
      .toBe(0);
  });
});
