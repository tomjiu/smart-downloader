/**
 * 全路径真实用户体检 e2e（health check）：
 * 在 critical-paths 之上补「按钮功能对齐 / 前后端对齐 / 全视图覆盖」层。
 *
 * 覆盖矩阵（每条 = 用户旅程 → UI 断言 + API 断言（部分 + 磁盘））：
 *   HC-0 首屏健康冒烟（/health /version + UI 骨架渲染）
 *   HC-1 侧栏 6 视图 + 任务过滤全巡检（aria-current / 关键元素渲染）
 *   HC-2 设置改读回环（UI 保存 → GET /settings API 值一致）
 *   HC-3 RSS 全链路（加订阅 → 刷新 → 条目 → 规则 → 删订阅）
 *   HC-4 批量操作（3 任务全选 → 批量暂停/恢复 → API 全验 → 批量删除）
 *   HC-5 任务详情按钮矩阵（重命名/标签/优先级/限速 → API 逐项回读）
 *   HC-6 错误路径（非法 URL 不崩 + 空态文案 + 404 API）
 *   HC-7 三方一致性（UI 状态 = API done/total = 磁盘文件大小）
 *   HC-8 SSE 实时性（无手动刷新，列表自动出现并推进到完成）
 *
 * 环境由 e2e/global-setup.ts 提供（E2E_BASE_URL / E2E_FILE_BASE）；
 * RSS 源来自 file-server.mjs /feed.xml（3 条确定性条目）。
 */
import { test, expect, type Page, type APIRequestContext } from '@playwright/test';
import { statSync } from 'node:fs';
import { join } from 'node:path';

const BASE = process.env.E2E_BASE_URL!;
const FILE = process.env.E2E_FILE_BASE!;
const FEED = `${FILE}/feed.xml`;

async function addTaskViaUi(page: Page, url: string, name?: string) {
  await page.getByTestId('new-task-btn').click();
  await page.getByTestId('add-url-input').fill(url);
  // 唯一显式名：串行共享 daemon 下，跨套件/同套件多任务的派生名会碰撞到同一
  // .part 文件（超合并验证 F5：并发互踩→凭据不可信→随机 Failed/瞬完）。
  // 显式名 = 独立落盘路径 = 确定性。
  if (name) await page.locator('#ntf-name').fill(name);
  await page.getByTestId('add-submit').click();
  await expect(page.getByTestId('add-url-input')).toBeHidden({ timeout: 10_000 });
}

/** API 侧取任务快照。 */
async function snap(request: APIRequestContext, id: string): Promise<Record<string, unknown>> {
  const r = await request.get(`${BASE}/tasks/${id}`);
  expect(r.ok()).toBeTruthy();
  return (await r.json()) as Record<string, unknown>;
}

/** 稳健全选：点表头全选（click 非 check，避免受控输入竞态 no-op），
 *  断言批量操作组出现计数徽标；失败重试（SSE 重渲染窗口竞态兜底）。 */
async function robustSelectAll(page: Page, want: number) {
  for (let i = 0; i < 5; i++) {
    await page.locator('th input[type="checkbox"]').click();
    try {
      await expect(page.getByLabel('批量操作').locator('.dl-tool-count').first()).toHaveText(
        String(want),
        { timeout: 1500 },
      );
      return;
    } catch {
      /* 未生效（点击落在重渲染窗口）→ 重试 */
    }
  }
  throw new Error(`robustSelectAll(${want}) 5 次尝试后仍未选中`);
}

/** 等全部任务达到给定状态集合（轮询 API）。 */
async function waitStates(
  request: APIRequestContext,
  want: Set<string>,
  timeoutMs = 30_000,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const r = await request.get(`${BASE}/tasks`);
    const tasks = (await r.json()) as Array<{ state: string }>;
    if (tasks.length > 0 && tasks.every((t) => want.has(t.state))) return;
    await new Promise((res) => setTimeout(res, 300));
  }
  throw new Error(`等待状态 ${[...want]} 超时`);
}

test.describe.serial('全路径体检 e2e（真实 daemon + 真实下载 + 全按钮）', () => {
  test.afterEach(async ({ request }) => {
    // 清场：删光任务 + RSS 订阅/规则，保住下一条用例干净起点
    const res = await request.get(`${BASE}/tasks`);
    if (res.ok()) {
      const tasks = (await res.json()) as Array<{ task_id: string }>;
      for (const t of tasks) {
        await request.delete(`${BASE}/tasks/${t.task_id}?delete_data=true`).catch(() => {});
      }
    }
    const rf = await request.get(`${BASE}/rss/feeds`).catch(() => null);
    if (rf && rf.ok()) {
      const { feeds } = (await rf.json()) as { feeds: Array<{ id: number }> };
      for (const f of feeds) await request.delete(`${BASE}/rss/feeds/${f.id}`).catch(() => {});
    }
    const rr = await request.get(`${BASE}/rss/rules`).catch(() => null);
    if (rr && rr.ok()) {
      const { rules } = (await rr.json()) as { rules: Array<{ id: number }> };
      for (const r of rules) await request.delete(`${BASE}/rss/rules/${r.id}`).catch(() => {});
    }
  });

  test('HC-0 首屏健康冒烟', async ({ page, request }) => {
    const health = await request.get(`${BASE}/health`);
    expect(health.ok()).toBeTruthy();
    const ver = await request.get(`${BASE}/version`);
    expect(ver.ok()).toBeTruthy();
    await page.goto(BASE!);
    // 侧栏品牌 + 主导航 + 空任务表（虚拟滚动契约：行数 0）
    await expect(page.locator('.dl-brand-name')).toBeVisible();
    await expect(page.getByLabel('主导航')).toBeVisible();
    await expect(page.getByTestId('task-row')).toHaveCount(0);
  });

  test('HC-1 侧栏 6 视图 + 过滤全巡检', async ({ page }) => {
    await page.goto(BASE!);
    const nav = page.getByLabel('视图与过滤');
    await expect(nav).toBeVisible();
    // 数据视图 4 个
    for (const label of ['统计', 'RSS', '日志', '安全']) {
      await nav.getByRole('button', { name: label }).click();
      await expect(
        page.getByRole('button', { name: label }).getAttribute('aria-current'),
      ).resolves.toBe('page');
    }
    // 设置（底部固定区）
    await page.getByRole('button', { name: '设置' }).click();
    await expect(page.getByLabel('搜索设置项')).toBeVisible();
    // 任务过滤组（qBittorrent 式）
    await page.getByRole('button', { name: '全部' }).click();
    await expect(page.getByTestId('task-row')).toHaveCount(0);
    // 视图关键元素抽检：统计页卡片 / 日志页过滤 tab / 安全页封禁表
    await page.getByRole('button', { name: '统计' }).click();
    await expect(page.getByText('统计', { exact: true }).first()).toBeVisible();
    await page.getByRole('button', { name: '日志' }).click();
    await expect(page.getByText('日志', { exact: true }).first()).toBeVisible();
    await page.getByRole('button', { name: '安全' }).click();
    // 空态：无封禁项时显示 EmptyState（bans-list 仅非空渲染——设计口径）
    await expect(page.getByText('暂无封禁项').first()).toBeVisible({ timeout: 10_000 });
  });

  test('HC-2 设置改读回环（UI 保存 = API 读回）', async ({ page, request }) => {
    await page.goto(BASE!);
    await page.getByRole('button', { name: '设置' }).click();
    // 带宽节：全局下载限速（数字输入）。先读现值
    const before = (await (await request.get(`${BASE}/settings`)).json()) as {
      bandwidth: { max_download_kb_s: number; max_upload_kb_s: number };
    };
    // 注意：SettingsView 的 label 未用 htmlFor 关联（a11y 发现，已记录）——
    // 用「label 文本 + 相邻 input」定位
    const input = page.locator('.dl-field-label:has-text("全局下载限速") + input');
    await expect(input).toBeVisible();
    await input.fill('777');
    await page.getByLabel('保存设置').click();
    // toast 回执
    await expect(page.getByText(/已落盘|已保存/).first()).toBeVisible({ timeout: 10_000 });
    // API 回读一致
    const after = (await (await request.get(`${BASE}/settings`)).json()) as {
      bandwidth: { max_download_kb_s: number };
    };
    expect(after.bandwidth.max_download_kb_s).toBe(777);
    // 恢复默认按钮（带宽节）
    await page.getByLabel('恢复带宽默认值').click();
    await page.getByLabel('保存设置').click();
    await expect(page.getByText(/已落盘|已保存/).first()).toBeVisible({ timeout: 10_000 });
    const restored = (await (await request.get(`${BASE}/settings`)).json()) as {
      bandwidth: { max_download_kb_s: number };
    };
    expect(restored.bandwidth.max_download_kb_s).toBe(before.bandwidth.max_download_kb_s);
  });

  test('HC-3 RSS 全链路（订阅 → 刷新 → 条目 → 规则 → 删除）', async ({ page, request }) => {
    await page.goto(BASE!);
    await page.getByRole('button', { name: 'RSS' }).click();
    // 加订阅
    const urlInput = page.getByPlaceholder('RSS/Atom 订阅 URL（https://…）');
    await urlInput.fill(FEED);
    await page.getByRole('button', { name: '添加订阅' }).click();
    await expect(page.getByText('HC Feed').first()).toBeVisible({ timeout: 15_000 });
    // 手动全部刷新（拉条目）
    await page.getByRole('button', { name: '全部刷新' }).click();
    await expect(page.getByText(/已刷新/).first()).toBeVisible({ timeout: 15_000 });
    // API：订阅存在 + 条目数 ≥ 3（file-server /feed.xml 固定 3 条）
    const feeds = (await (await request.get(`${BASE}/rss/feeds`)).json()) as {
      feeds: Array<{ id: number; title: string }>;
    };
    expect(feeds.feeds.length).toBeGreaterThanOrEqual(1);
    const feed = feeds.feeds.find((f) => f.title === 'HC Feed');
    expect(feed).toBeTruthy();
    const items = (await (await request.get(`${BASE}/rss/items?feed_id=${feed!.id}`)).json()) as {
      items: Array<{ title: string }>;
    };
    expect(items.items.length).toBeGreaterThanOrEqual(3);
    // UI 条目视图可见（按订阅过滤）
    await page.getByLabel('按订阅过滤条目').selectOption(String(feed!.id)).catch(async () => {
      // select 不可用时（组件形态差异）直接断言条目标题已在页面
      await expect(page.getByText('HC 第 1 集 720p').first()).toBeVisible({ timeout: 10_000 });
    });
    // 添加规则（must_contain 正则匹配「第 \d+ 集」）
    const nameInput = page.getByPlaceholder(/规则名|名称/);
    if ((await nameInput.count()) > 0) {
      await nameInput.first().fill('HC 正集');
      const must = page.getByPlaceholder(/必须包含|must contain/i).first();
      await must.fill('第 \\d+ 集');
      const regexToggle = page.getByLabel('使用正则表达式');
      if (await regexToggle.count()) await regexToggle.click();
      await page.getByRole('button', { name: /添加规则/ }).click();
      await expect(page.getByText('HC 正集').first()).toBeVisible({ timeout: 10_000 });
      const rules = (await (await request.get(`${BASE}/rss/rules`)).json()) as {
        rules: Array<{ name: string }>;
      };
      expect(rules.rules.some((r) => r.name === 'HC 正集')).toBeTruthy();
    }
    // 删除订阅（确认框）
    await page.getByRole('button', { name: /删除/ }).first().click();
    const confirm = page.getByRole('alertdialog');
    if (await confirm.count()) {
      await page.getByTestId('confirm-ok').click();
    }
    await expect(page.getByText('HC Feed')).toHaveCount(0, { timeout: 10_000 });
  });

  test('HC-4 批量操作（暂停/恢复/删除 ×3 任务）', async ({ page, request }) => {
    await page.goto(BASE!);
    // 3 个慢速任务（512 KiB/s → 持续 Downloading 窗口充足）；
    // 唯一显式名：三个任务各自独立 .part，不再互踩（也不吃 critical #4
    // UI 删除（keep_data 默认）残留的 delete.bin.part 断点）
    for (let i = 0; i < 3; i++) {
      await addTaskViaUi(page, `${FILE}/delete.bin?hc=${i}`, `hc4-batch-${i}.bin`);
    }
    await expect(page.getByTestId('task-row')).toHaveCount(3, { timeout: 15_000 });
    await waitStates(request, new Set(['Downloading']), 20_000);
    // 批量操作组（作用域限定工具栏，避开行内同名按钮）
    const batch = page.getByLabel('批量操作');
    // 全选 → 批量暂停
    await robustSelectAll(page, 3);
    await batch.getByRole('button', { name: /暂停/ }).click();
    await waitStates(request, new Set(['Paused']), 10_000);
    // UX-1 已修复（fix/batch-keep-selection）：pause/resume 保留选择集——
    // 断言计数徽标仍为 3（无需重新全选），直接连击恢复
    await expect(batch.locator('.dl-tool-count').first()).toHaveText('3');
    await batch.getByRole('button', { name: /恢复/ }).click();
    await waitStates(request, new Set(['Downloading']), 10_000);
    // 批量删除（确认）——remove 仍清空选择（任务已删），重选
    await robustSelectAll(page, 3);
    await batch.getByRole('button', { name: /删除/ }).click();
    await page.getByTestId('confirm-ok').click();
    await expect(page.getByTestId('task-row')).toHaveCount(0, { timeout: 15_000 });
  });

  test('HC-5 任务详情按钮矩阵（重命名/标签/优先级/限速 → API 回读）', async ({ page, request }) => {
    await page.goto(BASE!);
    await addTaskViaUi(page, `${FILE}/delete.bin?hc5`, 'hc5-detail.bin');
    await expect(page.getByTestId('task-row')).toHaveCount(1, { timeout: 15_000 });
    const id = (await (await request.get(`${BASE}/tasks`)).json())[0].task_id as string;
    // 打开详情（双击行——产品口径，见 critical-paths #6）
    await page.getByTestId('task-row').first().dblclick();
    await expect(page.getByLabel('任务详情')).toBeVisible();
    // 1) 重命名
    await page.getByTestId('rename-start').click();
    await page.getByTestId('rename-input').fill('体检重命名');
    await page.getByTestId('rename-save').click();
    let s = await snap(request, id);
    expect(s.name).toBe('体检重命名');
    // 2) 标签
    await page.getByTestId('tags-edit').click();
    await page.getByTestId('tags-input').fill('hc, audit');
    await page.getByTestId('tags-save').click();
    s = await snap(request, id);
    expect(s.tags).toEqual(['hc', 'audit']);
    // 3) 队列优先级（顶置）——【体检发现 C-1】queue_priority 仅 set 响应回显，
    //    TaskSnapshot/TaskSummary 均不持久透出（重启 UI 即失显）→ 用响应捕获断言
    const prioResp = page.waitForResponse(
      (r) => r.url().includes('/priority') && r.request().method() === 'POST',
    );
    await page.getByTestId('prio-top').click();
    const prio = (await (await prioResp).json()) as { queue_priority: number };
    // 语义：数值越小越先出队（top = 队列最小值 - 1）——单任务默认 0 → top 得 -1
    expect(prio.queue_priority).toBeLessThan(0);
    // 4) 任务限速
    await page.getByTestId('limit-down').fill('256');
    await page.getByTestId('apply-limit').click();
    s = await snap(request, id);
    expect((s.limits as { down_kb_s?: number })?.down_kb_s).toBe(256);
    // Esc 关闭详情（分层焦点）
    await page.keyboard.press('Escape');
    await expect(page.getByLabel('任务详情')).toBeHidden();
  });

  test('HC-6 错误路径（非法输入不崩 + 空态 + 404）', async ({ page, request }) => {
    await page.goto(BASE!);
    // 非法 URL：daemon 拒绝 → UI 错误 toast，页面不崩
    await page.getByTestId('new-task-btn').click();
    await page.getByTestId('add-url-input').fill('not-a-url');
    await page.getByTestId('add-submit').click();
    // 输入条保持展开（失败保留稿），错误提示可见
    await expect(page.getByTestId('add-url-input')).toBeVisible();
    await expect(page.locator('.dl-toast--error, [role="alert"]').first()).toBeVisible({
      timeout: 10_000,
    });
    await page.keyboard.press('Escape');
    // 搜索空结果 → 空态文案
    await page.getByTestId('search-input').fill('不存在的任务xyz');
    await expect(page.getByTestId('task-row')).toHaveCount(0);
    await expect(page.getByText(/没有匹配|无匹配|空/).first()).toBeVisible();
    // API 404：不存在任务
    const r = await request.get(`${BASE}/tasks/nonexistent-id`);
    expect(r.status()).toBe(404);
  });

  test('HC-7 三方一致性（UI = API = 磁盘）', async ({ page, request }) => {
    await page.goto(BASE!);
    // 唯一显式名：磁盘断言可精确指向目标文件（dirFiles[0] 扫描会抓到其他
    // 套件残留的 .part/.progress 孤儿——bisect2 中 HC-7 即因此误挂）
    await addTaskViaUi(page, `${FILE}/small.bin`, 'hc7-check.bin');
    await waitStates(request, new Set(['Completed']), 30_000);
    // UI：状态列已完成（API 状态名 Completed——扁平字符串口径，曾误写 Done）
    await expect(page.getByTestId('task-state').first()).toHaveText(/完成|Completed/);
    // API：done = total = 1MiB（TaskSummary 无进度字段——须走 snapshot）
    const tasks = (await (await request.get(`${BASE}/tasks`)).json()) as Array<{
      task_id: string;
    }>;
    const s = (await (await request.get(`${BASE}/tasks/${tasks[0].task_id}`)).json()) as {
      dest_root: string;
      done: number;
      total: number;
      files: Array<{ rel_path?: string }>;
    };
    expect(s.total).toBe(1024 * 1024);
    expect(s.done).toBe(s.total);
    // 磁盘：显式名 → 路径可推导（不再扫描目录首个文件）
    // 【体检发现 C-2】HTTP 单文件任务 files 恒空——落盘路径不透出；显式名
    // 即 dest_root/<name>，确定性断言取代脆弱扫描。
    let p = join(s.dest_root, 'hc7-check.bin');
    const st = statSync(p);
    expect(st.isFile()).toBeTruthy();
    expect(st.size).toBe(1024 * 1024);
  });

  test('HC-8 SSE 实时性（零手动刷新自动推进）', async ({ page, request }) => {
    await page.goto(BASE!);
    await expect(page.getByTestId('task-row')).toHaveCount(0);
    await addTaskViaUi(page, `${FILE}/small.bin`, 'hc8-sse.bin');
    // 不刷新页面：行自动出现（SSE task_created）并推进到 Done
    await expect(page.getByTestId('task-row')).toHaveCount(1, { timeout: 10_000 });
    await expect(page.getByTestId('task-state').first()).toHaveText(/完成|Completed/, {
      timeout: 30_000,
    });
    // 日志视图同步可见事件（SSE 推送链路）——事件行在 .dl-card 内（.dl-badge 类型徽章）
    await page.getByRole('button', { name: '日志' }).click();
    await expect(page.locator('.dl-card .dl-badge').first()).toBeVisible({
      timeout: 10_000,
    });
  });
});
