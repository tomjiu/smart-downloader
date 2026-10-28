/**
 * 日志页事件流 + 统计页速度曲线 e2e（体检盲区补全：#182/#202 后续扫描）。
 *
 * 背景：HC 系列（#181）覆盖 6 视图巡检与任务列表 SSE 实时性，但两处
 * 用户可见面仍零 e2e 护栏：
 *   - LogsView（日志页）：SSE → events 状态 → 类型徽章/摘要渲染、类型
 *     tab 过滤（aria-pressed + 计数收缩）——事件管道的「最后一公里」；
 *   - SpeedChart（统计页）：historyRef 变速点累积 → SVG 曲线渲染——
 *     <2 个变速点时曲线静默不画（downPath 为空串），数据管道断裂无
 *     任何报错，只有 e2e 能拦。
 *
 * 锚点说明（无 data-testid 的既有结构，均用稳定语义选择器）：
 *   - 事件条目：`.dl-badge`（类型徽章文本 = 事件 type）+ 摘要文本
 *     （summarize() 的确定性输出，如「任务创建 <id>」「完成 <id>」）；
 *   - tab：`aria-pressed` + `dl-tab` 类；计数：`显示 X / Y 条`文本；
 *   - down 曲线：`path[stroke="var(--accent)"]` 且 `d` 以 M 开头。
 *
 * 数据面：small.bin（1MB 不限，秒完成）驱动事件流用例；limit.bin
 * （128MB @4MiB/s×2worker ≈16s 活跃）驱动速度曲线用例——实测 /stats
 * 采样：0↔16MB/s 尖峰交替（16MB 段粒度），16s 内 7 个变速点；pause.bin
 * （32MB ≈4s）尖峰可能整段落在两次 1s 轮询之间被漏采（实测 0 变速点），
 * 不可用。
 */
import { test, expect, type Page, type APIRequestContext } from '@playwright/test';

const BASE = process.env.E2E_BASE_URL!;
const FILE = process.env.E2E_FILE_BASE!;

async function addTaskViaUi(page: Page, url: string) {
  await page.getByTestId('new-task-btn').click();
  await page.getByTestId('add-url-input').fill(url);
  await page.getByTestId('add-submit').click();
  await expect(page.getByTestId('add-url-input')).toBeHidden({ timeout: 15_000 });
}

/** 解析「显示 X / Y 条」计数。 */
async function readCounts(page: Page): Promise<{ shown: number; total: number }> {
  const txt = await page.getByText(/显示 \d+ \/ \d+ 条/).textContent();
  const m = /显示 (\d+) \/ (\d+) 条/.exec(txt ?? '');
  expect(m, `计数文本可解析（实际：${txt}）`).toBeTruthy();
  return { shown: Number(m![1]), total: Number(m![2]) };
}

test.describe.serial('日志页事件流 + 速度曲线（体检盲区补全）', () => {
  test.afterEach(async ({ request }) => {
    const res = await request.get(`${BASE}/tasks`);
    if (res.ok()) {
      const tasks = (await res.json()) as Array<{ task_id: string }>;
      for (const t of tasks) {
        await request.delete(`${BASE}/tasks/${t.task_id}?delete_data=true`).catch(() => {});
      }
    }
  });

  test('1. 日志页事件流实况：徽章/摘要渲染 + 类型 tab 过滤 + 计数', async ({
    page,
    request,
  }) => {
    // 1) UI 加任务（small.bin 秒完成）→ 拿 task_id
    await page.goto(BASE!);
    await addTaskViaUi(page, `${FILE}/small.bin`);
    const row = page.getByTestId('task-row').first();
    await expect(row).toBeVisible({ timeout: 10_000 });
    await expect(row).toHaveAttribute('data-task-state', /Completed|Seeding/, { timeout: 20_000 });
    const list = (await request.get(`${BASE}/tasks`).then((r) => r.json())) as Array<{
      task_id: string;
    }>;
    const id = list[0]?.task_id;
    expect(id).toBeTruthy();

    // 2) 切日志页：本任务的创建/完成事件可见（SSE → events → 渲染全链；
    //    摘要文本是 summarize() 确定性输出，非模糊包含）
    await page.getByRole('button', { name: '日志' }).click();
    await expect(page.getByText(`任务创建 ${id}`, { exact: false })).toBeVisible({
      timeout: 10_000,
    });
    await expect(page.getByText(`完成 ${id}`, { exact: false })).toBeVisible({
      timeout: 10_000,
    });
    // 类型徽章真实渲染（.dl-badge 承载事件 type 文本）
    await expect(page.locator('.dl-badge', { hasText: 'task_created' }).first()).toBeVisible();
    const before = await readCounts(page);
    expect(before.total).toBeGreaterThanOrEqual(2);
    expect(before.shown).toBe(before.total);

    // 3) 类型 tab 过滤：点 completed → aria-pressed + 计数收缩
    const completedTab = page.locator('.dl-tab', { hasText: 'completed' }).first();
    await completedTab.click();
    await expect(completedTab).toHaveAttribute('aria-pressed', 'true');
    const filtered = await readCounts(page);
    expect(filtered.shown).toBeLessThan(filtered.total);
    expect(filtered.shown).toBeGreaterThanOrEqual(1);
    // 过滤后可见条目全部是 completed 徽章（每行恰一枚 .dl-badge）
    const badges = page.locator('.dl-card .dl-badge');
    await expect(badges.first()).toBeVisible();
    const badgeTexts = (await badges.allTextContents()).map((s) => s.trim());
    for (const b of badgeTexts) expect(b).toBe('completed');

    // 4)「全部」恢复：计数还原 + aria-pressed 归位
    const allTab = page.locator('.dl-tab', { hasText: '全部' }).first();
    await allTab.click();
    await expect(allTab).toHaveAttribute('aria-pressed', 'true');
    const restored = await readCounts(page);
    expect(restored.shown).toBe(restored.total);
  });

  test('2. 速度曲线渲染护栏：活跃下载变速点 → 统计页 SVG 曲线非空', async ({
    page,
    request,
  }) => {
    test.setTimeout(60_000);
    // 1) 加 limit.bin（128MB ≈16s 活跃；实测 /stats 采样 0↔16MB/s 交替
    //    → historyRef 每 poll 变点累积；段粒度 16MB）→ 等下载中
    await page.goto(BASE!);
    await addTaskViaUi(page, `${FILE}/limit.bin`);
    const row = page.getByTestId('task-row').first();
    await expect(row).toBeVisible({ timeout: 10_000 });
    await expect(row).toHaveAttribute('data-task-state', 'Downloading', { timeout: 20_000 });

    // 2) 变速点累积窗口（实测尖峰约每 4s 一次，下载全程 ~16s；先等 3s
    //    让首批尖峰入账，断言侧再靠 toBeVisible 自动重试捕获后续尖峰——
    //    曲线随新点实时重绘，无需精确卡点）
    await page.waitForTimeout(3_000);

    // 3) 切统计页：SpeedChart 渲染——down 曲线 path d="M…" 非空
    //    （<2 变速点时 downPath 为空串 → path 不渲染 → 此处必红，
    //     即「数据管道静默断裂」的护栏；15s 重试窗口覆盖 ≥3 次尖峰）
    await page.getByRole('button', { name: '统计' }).click();
    const svg = page.locator('svg[viewBox="0 0 640 180"]');
    await expect(svg).toBeVisible({ timeout: 5_000 });
    const downPath = svg.locator('path[stroke="var(--accent)"]');
    await expect(downPath.first()).toBeVisible({ timeout: 15_000 });
    const d = await downPath.first().getAttribute('d');
    expect(d, 'down 曲线 path 非空且为合法 SVG 路径').toMatch(/^M/);

    // 4) 收尾前任务仍在（afterEach 统一清场；此处不额外断言状态——
    //    曲线已入账 historyRef，完成与否不影响已渲染曲线）
    const list = (await request.get(`${BASE}/tasks`).then((r) => r.json())) as Array<{
      task_id: string;
    }>;
    expect(list.length).toBeGreaterThanOrEqual(1);
  });
});
