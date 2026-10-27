/**
 * R17 键盘导航 e2e（真实 daemon + 真实 UI 产物）：↑/↓/Enter/Space +
 * 虚拟滚动跨窗口接力（回归护栏——实现已在但零测试覆盖，虚拟窗口与
 * 焦点接力是回归风险点：#133 虚拟滚动合入期键盘路径最易碎）。
 *
 * 覆盖点：
 *   1. ↑/↓ 焦点移动（document.activeElement 的 data-idx 逐行断言）
 *   2. 跨窗口导航：30 任务（默认视口 ~18 行可见），↓ 连续推进到窗口外
 *      行 idx=25——先滚动再聚焦（focusIdx effect 接力），焦点不丢
 *   3. Enter 开详情（焦点行 = 打开的行）
 *   4. Space 选中（行 is-selected + 全选 checkbox 进 indeterminate）
 */
import { test, expect, type Page } from '@playwright/test';

const BASE = process.env.E2E_BASE_URL!;
const FILE = process.env.E2E_FILE_BASE!;
const N = 30;

test.describe.serial('键盘导航 e2e（R17：↑/↓/Enter/Space + 虚拟窗口接力）', () => {
  test.afterEach(async ({ request }) => {
    const res = await request.get(`${BASE}/tasks`);
    if (!res.ok()) return;
    const tasks = (await res.json()) as Array<{ task_id: string }>;
    for (let i = 0; i < tasks.length; i += 100) {
      await request
        .post(`${BASE}/tasks/batch`, {
          data: {
            action: 'remove',
            ids: tasks.slice(i, i + 100).map((t) => t.task_id),
            delete_data: true,
          },
        })
        .catch(() => {});
    }
  });

  /** 当前焦点行的 data-idx（无焦点行 = null）。 */
  async function focusIdx(page: Page): Promise<number | null> {
    return page.evaluate(() => {
      const el = document.activeElement as HTMLElement | null;
      const idx = el?.closest?.('tr[data-idx]')?.getAttribute('data-idx');
      return idx == null ? null : Number(idx);
    });
  }

  test('↓ 逐行聚焦 → 跨窗口接力（idx=25）→ Enter 开详情 → Space 选中', async ({
    page,
    request,
  }) => {
    // 30 任务（默认视口约 18 行可见 + overscan——idx 20+ 在窗口外）
    for (let i = 1; i <= N; i++) {
      const res = await request.post(`${BASE}/tasks`, {
        data: { url: `${FILE}/bulk/${i}.bin`, name: `kb-${String(i).padStart(2, '0')}.bin` },
      });
      expect(res.ok()).toBeTruthy();
    }
    await page.goto(BASE);
    await expect
      .poll(async () => page.getByTestId('task-row').count(), { timeout: 20_000 })
      .toBeGreaterThan(10);

    // 聚焦首行（行内点击选中副作用最小路径：直接 focus()）
    await page.getByTestId('task-row').first().focus();
    expect(await focusIdx(page)).toBe(0);

    // ↓ ×3：焦点逐行推进
    for (const want of [1, 2, 3]) {
      await page.keyboard.press('ArrowDown');
      expect(await focusIdx(page)).toBe(want);
    }
    // ↑ 回退
    await page.keyboard.press('ArrowUp');
    expect(await focusIdx(page)).toBe(2);

    // 跨窗口接力：从 idx=2 连按 ↓ 到 idx=25（远超默认窗口 ~18 行）——
    // 每步焦点必须落位（滚动跟随 + focusIdx effect 接管渲染后的行）
    for (let i = 3; i <= 25; i++) {
      await page.keyboard.press('ArrowDown');
      const got = await focusIdx(page);
      if (got !== i) {
        // 窗口刚扩展的一帧可能尚未渲染目标行——effect 接力在下一帧；
        // 给一拍重试（连续两拍错位才是真回归）
        await page.waitForTimeout(100);
        const retry = await focusIdx(page);
        expect(retry, `ArrowDown 至 idx=${i}：焦点丢（got ${got} → retry ${retry}）`).toBe(i);
      }
    }
    // 滚动跟随：目标行已在视口内（其 getBoundingClientRect 在表格容器内）
    const inView = await page.evaluate(() => {
      const el = document.activeElement as HTMLElement;
      const row = el.closest('tr');
      const wrap = document.querySelector('.dl-table-wrap') ?? document.querySelector('.dl-tbl-wrap');
      if (!row || !wrap) return false;
      const r = row.getBoundingClientRect();
      const w = wrap.getBoundingClientRect();
      return r.bottom > w.top && r.top < w.bottom;
    });
    expect(inView, '焦点行必须滚动到容器视口内').toBeTruthy();

    // Enter：开详情（抽屉出现 + 标题含焦点行任务名 kb-26）
    await page.keyboard.press('Enter');
    await expect(page.getByText('kb-26.bin').first()).toBeVisible({ timeout: 10_000 });
    // 关抽屉（Esc）→ 焦点归还触发行（R17 WAI-ARIA 对话框模式）
    await page.keyboard.press('Escape');
    await expect
      .poll(async () => focusIdx(page), { timeout: 5_000 })
      .toBe(25, 'Esc 关抽屉后焦点必须归还触发行（idx=25）');

    // Space：选中当前焦点行（idx=25 → kb-26；焦点已由 Esc 归还到位）
    expect(await focusIdx(page)).toBe(25);
    await page.keyboard.press(' ');
    await expect(
      page.locator('tr[data-idx="25"]').first(),
    ).toHaveClass(/is-selected/);
    // 全选框进入 indeterminate 态（部分选中）
    await expect(page.getByRole('checkbox', { name: /全选/ }).first()).toHaveJSProperty(
      'indeterminate',
      true,
    );
  });
});
