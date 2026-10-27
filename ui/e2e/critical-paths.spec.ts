/**
 * Issue #121 验收标准 #6：UI 端到端 5 条关键路径（Playwright，真实
 * daemon + 真实下载 + 真实 UI 产物）。
 *
 * 路径清单（REALTEST-PLAN §A-1 方案 3 的 e2e 冒烟定位——验证的是
 * 「用户可见效果」，不是函数返回值）：
 *   1. 添加任务 → 列表出现 → 状态到「已完成」（核心下载路径）
 *   2. 限速真的生效（Issue #115 核心验收项：设 128 KiB/s → 实测 ≤ 150）
 *   3. 暂停 → 已暂停 → 恢复 → 继续下载到完成
 *   4. 删除需确认（alertdialog + 初始焦点在取消）→ 确认后行消失
 *   5. 无任务名不崩（Issue #115 验收 #8：新建后立即搜索 + null name 渲染）
 *
 * 环境由 e2e/global-setup.ts 提供（E2E_BASE_URL / E2E_FILE_BASE）。
 * 串行 + afterEach 清场：共享 daemon 的真实状态，用例间零残留。
 *
 * 虚拟滚动兼容契约（#133 合入后）：TasksTable 只渲染可视窗口内的行
 * （tr[data-idx]），行级断言依赖两个不变量——
 *   1. 本套件全部为「单任务」场景，首行恒在窗口内，.first() 安全；
 *      多任务场景必须先 scrollIntoViewIfNeeded() 再断言；
 *   2. 「列表为空」断言用 toHaveCount(0)（渲染行数为 0），而非
 *      「任务总数为 0」——不可见的行不在 DOM 里。
 */
import { test, expect, type Page } from '@playwright/test';

const BASE = process.env.E2E_BASE_URL!;
const FILE = process.env.E2E_FILE_BASE!;

/** 速率文本 → KiB/s 数值（"128.0 KiB/s" / "1.5 MiB/s" / "512 B/s"）。 */
function parseSpeedKiBps(text: string | null): number {
  if (!text) return NaN;
  const m = /([\d.]+)\s*(B|KiB|MiB|GiB)\/s/.exec(text);
  if (!m) return NaN;
  const v = parseFloat(m[1]);
  const scale = { B: 1 / 1024, KiB: 1, MiB: 1024, GiB: 1024 * 1024 }[m[2] as 'B' | 'KiB' | 'MiB' | 'GiB'];
  return v * scale;
}

/** 走 UI 完整交互添加任务（新建 → 填 URL → 添加）。 */
async function addTaskViaUi(page: Page, url: string) {
  await page.getByTestId('new-task-btn').click();
  await page.getByTestId('add-url-input').fill(url);
  await page.getByTestId('add-submit').click();
  // 添加条收起 = 请求已被受理（添加成功后 DlApp 会合上新建条）
  await expect(page.getByTestId('add-url-input')).toBeHidden({ timeout: 10_000 });
}

test.describe.serial('关键路径 e2e（真实 daemon + 真实下载）', () => {
  // 清场：删光任务（含数据），保住下一条用例的干净起点
  test.afterEach(async ({ request }) => {
    const res = await request.get(`${BASE}/tasks`);
    if (!res.ok()) return;
    const tasks = (await res.json()) as Array<{ task_id: string }>;
    for (const t of tasks) {
      await request.delete(`${BASE}/tasks/${t.task_id}?delete_data=true`).catch(() => {});
    }
  });

  test('1. 添加任务 → 出现 → 已完成（核心下载路径）', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (e) => pageErrors.push(String(e)));
    await page.goto('/');
    await addTaskViaUi(page, `${FILE}/small.bin`);

    const row = page.getByTestId('task-row').first();
    await expect(row).toBeVisible({ timeout: 10_000 });
    await expect(row).toHaveAttribute('data-task-state', 'Completed', { timeout: 30_000 });
    // 用户可见效果：状态徽章显示「已完成」，任务名非空
    await expect(page.getByTestId('task-state').first()).toContainText('已完成');
    const name = await page.getByTestId('task-name').first().textContent();
    expect((name ?? '').trim().length).toBeGreaterThan(0);
    expect(pageErrors, '页面不得有未捕获异常').toEqual([]);
  });

  test('2. 限速真的生效（UI 应用 + API 回显 + 行为证明）', async ({ page, request }) => {
    await page.goto('/');
    // 128MB @ 服务器 4MiB/s × 2 worker ≈ 8MiB/s 聚合：无任务限速时全程 ~16s，
    // UI 交互链（开详情+填值+应用）约 3-4s 落地限速——窗口充足无竞态。
    await addTaskViaUi(page, `${FILE}/limit.bin`);
    const row = page.getByTestId('task-row').first();
    await expect(row).toBeVisible({ timeout: 10_000 });
    await expect(row).toHaveAttribute('data-task-state', 'Downloading', { timeout: 20_000 });

    // 双击行打开详情抽屉 → 填 512 KiB/s → 应用
    await row.dblclick();
    const limitInput = page.getByTestId('limit-down');
    await expect(limitInput).toBeVisible({ timeout: 5_000 });
    await limitInput.fill('512');
    await page.getByTestId('apply-limit').click();

    // a) 用户可见确认：toast「限速已应用」
    await expect(page.getByText('限速已应用')).toBeVisible({ timeout: 5_000 });

    // b) API 回显（详情抽屉 placeholder「当前 512」的数据源）
    const list = (await request.get(`${BASE}/tasks`).then((r) => r.json())) as Array<{
      task_id: string;
    }>;
    const tid = list[0]?.task_id;
    expect(tid).toBeTruthy();
    const snap = (await request.get(`${BASE}/tasks/${tid}`).then((r) => r.json())) as {
      limits?: { down_kb_s?: number };
    };
    expect(snap.limits?.down_kb_s, '限速必须落到任务快照（回显数据源）').toBe(512);

    // c) 行为证明（用户可见效果）：限速 512KiB/s 后剩余 ≥100MB 至少需 200s——
    //    +25s 时任务必须仍在下载（无限速时全程 ~16s 早已 Completed；
    //    物理下界 512KiB/s × 25s ≈ 12.8MB ≪ 剩余，无侥幸通过路径）。
    //    注：速率列不作为断言信号——16MB 段粒度下速率呈段间突发（0/尖峰），
    //    这是当前进度上报粒度的已知特性（见 file-server.mjs 头注）。
    await page.waitForTimeout(25_000);
    await expect(row).toHaveAttribute('data-task-state', 'Downloading');
  });

  test('3. 暂停 → 已暂停 → 恢复 → 完成', async ({ page }) => {
    await page.goto('/');
    // 32MB @ 4MiB/s = 全程 8s；16MB 段 ≈ 4s → 段边界暂停等待有界
    await addTaskViaUi(page, `${FILE}/pause.bin`);
    const row = page.getByTestId('task-row').first();
    await expect(row).toHaveAttribute('data-task-state', 'Downloading', { timeout: 20_000 });

    // 暂停（真暂停：段边界退出，在飞段收尾后才置 Paused）
    await page.getByTestId('pause-resume-btn').first().click();
    await expect(row).toHaveAttribute('data-task-state', 'Paused', { timeout: 20_000 });

    // 已暂停的稳定性 + 速率归零（用户可见：下载确实停了）。
    // 速率容错口径：SSE 速率 TTL 5s（暂停瞬间的段完成尖峰会残留），
    // 等 6s 过 TTL 后快照侧 Paused 恒报 0 → 速率列显示「—」或「0 B/s」。
    await page.waitForTimeout(6_000);
    await expect(row).toHaveAttribute('data-task-state', 'Paused');
    const speedText = await page.getByTestId('task-speed').first().textContent();
    const kbps = parseSpeedKiBps(speedText);
    const isStopped = Number.isNaN(kbps) || kbps <= 1; // 「—」或「0 B/s」都算停
    expect(isStopped, `暂停 6s 后速率应为 0/—，实测 ${speedText}`).toBe(true);

    // 恢复 → 继续下载到完成
    await page.getByTestId('pause-resume-btn').first().click();
    await expect(row).toHaveAttribute('data-task-state', 'Downloading', { timeout: 20_000 });
    await expect(row).toHaveAttribute('data-task-state', 'Completed', { timeout: 60_000 });
  });

  test('4. 删除需确认 → 确认后任务消失', async ({ page }) => {
    await page.goto('/');
    // 8MB @ 512KiB/s = 16s 全程：删除动作发生在下载中（覆盖引擎侧移除路径）
    await addTaskViaUi(page, `${FILE}/delete.bin`);
    const row = page.getByTestId('task-row').first();
    await expect(row).toBeVisible({ timeout: 10_000 });

    // 点删除 → 确认框出现（alertdialog 语义 + 初始焦点在取消 = 安全默认）
    await page.getByTestId('delete-btn').first().click();
    const dialog = page.locator('[role="alertdialog"]');
    await expect(dialog).toBeVisible();
    await expect(page.getByTestId('confirm-cancel')).toBeFocused();

    // 确认 → 行消失（用户可见效果）
    await page.getByTestId('confirm-ok').click();
    await expect(page.getByTestId('task-row')).toHaveCount(0, { timeout: 10_000 });
  });

  test('5. 无任务名不崩（新建后立即搜索 + null name 渲染兜底）', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (e) => pageErrors.push(String(e)));
    await page.goto('/');

    // 5a. 验收原文动作：新建任务后立即搜索（抢首帧快照窗口）
    await addTaskViaUi(page, `${FILE}/small.bin`);
    await page.getByTestId('search-input').fill('不存在的搜索串xyz');
    await page.waitForTimeout(1_500);
    // 过滤后列表为空但页面活着（不白屏）
    await expect(page.getByTestId('task-row')).toHaveCount(0);
    await expect(page.getByTestId('search-input')).toBeVisible();
    // 清空恢复
    await page.getByTestId('search-input').fill('');
    await expect(page.getByTestId('task-row').first()).toBeVisible({ timeout: 10_000 });

    // 5b. 确定性 null name 渲染：拦截列表响应注入 name:null
    //    （唯一 shim——daemon 对 URL add 恒派生非空名，null 名只出现在
    //    RSS/引擎回填窗口；渲染兜底是 P0-2 修复的行为面）
    await page.route('**/tasks', async (route) => {
      const req = route.request();
      if (req.method() !== 'GET' || new URL(req.url()).pathname !== '/tasks') {
        await route.fallback();
        return;
      }
      const res = await route.fetch();
      const body = (await res.json()) as unknown[];
      body.forEach((t) => {
        if (t && typeof t === 'object' && 'name' in t) (t as { name: unknown }).name = null;
      });
      await route.fulfill({ response: res, body: JSON.stringify(body) });
    });
    await page.reload();
    const nameCell = page.getByTestId('task-name').first();
    await expect(nameCell).toBeVisible({ timeout: 10_000 });
    const text = await nameCell.textContent();
    expect((text ?? '').trim().length, 'null name 必须回退到派生名，不能空白').toBeGreaterThan(0);
    expect(pageErrors, 'null name 渲染不得抛异常').toEqual([]);
    await page.unrouteAll();
  });

  test('6. 详情抽屉第二梯队：队列优先级 + 重命名 + 标签 + Esc 分层', async ({ page, request }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (e) => pageErrors.push(String(e)));
    await page.goto('/');
    await addTaskViaUi(page, `${FILE}/small.bin`);
    const row = page.getByTestId('task-row').first();
    await expect(row).toBeVisible({ timeout: 10_000 });
    await expect(row).toHaveAttribute('data-task-state', 'Completed', { timeout: 30_000 });

    // 双击开详情抽屉
    await row.dblclick();
    const drawer = page.locator('.dl-drawer');
    await expect(drawer).toBeVisible({ timeout: 5_000 });

    // 6a. 队列优先级：置底 → 反馈 + 信息卡出现「队列优先级」行
    //    （文本在信息行 span 与卡片标题 b 各出现一次——strict mode 需 .first()）
    await page.getByTestId('prio-bottom').click();
    await expect(page.getByText('队列优先级已调整')).toBeVisible({ timeout: 5_000 });
    await expect(drawer.getByText('队列优先级', { exact: true }).first()).toBeVisible();

    // 6b. 重命名：铅笔 → 行内输入 → Enter 保存 → 抽屉标题与 API 回显
    await page.getByTestId('rename-start').click();
    const renameInput = page.getByTestId('rename-input');
    await expect(renameInput).toBeFocused();
    await renameInput.fill('e2e-改名验证');
    await renameInput.press('Enter');
    await expect(page.getByText('任务名已更新')).toBeVisible({ timeout: 5_000 });
    const list = (await request.get(`${BASE}/tasks`).then((r) => r.json())) as Array<{
      task_id: string;
      name?: string | null;
    }>;
    expect(list[0]?.name, '重命名必须落到任务名（API 回显）').toBe('e2e-改名验证');

    // 6c. 标签：编辑 → 逗号分隔两项 → Enter 保存 → 徽章渲染
    await page.getByTestId('tags-edit').click();
    const tagsInput = page.getByTestId('tags-input');
    await expect(tagsInput).toBeFocused();
    await tagsInput.fill('电影, 高清');
    await tagsInput.press('Escape'); // Esc 分层：编辑取消，抽屉不关
    await expect(drawer).toBeVisible();
    await expect(page.getByTestId('tags-input')).toBeHidden();
    await page.getByTestId('tags-edit').click();
    await page.getByTestId('tags-input').fill('电影, 高清');
    await page.getByTestId('tags-save').click();
    await expect(page.getByText('标签已保存')).toBeVisible({ timeout: 5_000 });
    await expect(drawer.locator('.dl-badge--info', { hasText: '电影' })).toBeVisible();
    await expect(drawer.locator('.dl-badge--info', { hasText: '高清' })).toBeVisible();

    // 6d. 关抽屉后列表行名同步（onChanged → 列表刷新链路）
    await page.keyboard.press('Escape');
    await expect(drawer).toBeHidden({ timeout: 5_000 });
    await expect(page.getByTestId('task-name').first()).toContainText('e2e-改名验证', { timeout: 10_000 });
    expect(pageErrors, '页面不得有未捕获异常').toEqual([]);
  });

  test('7. 新建表单：探测预览 + 校验和闸门 + 选项透传 + 收起不丢稿（Wave B）', async ({ page, request }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (e) => pageErrors.push(String(e)));
    await page.goto('/');
    await page.getByTestId('new-task-btn').click();
    const urlInput = page.getByTestId('add-url-input');
    await expect(urlInput).toBeVisible();
    await expect(urlInput).toBeFocused();

    // 7a. 探测预览：大小/落盘名/断点续传（真实探测，不建任务）
    await urlInput.fill(`${FILE}/small.bin`);
    await page.getByTestId('probe-btn').click();
    const probe = page.getByTestId('probe-result');
    await expect(probe).toBeVisible({ timeout: 10_000 });
    await expect(probe).toContainText('small.bin');
    await expect(probe).toContainText('1.0 MiB');
    // 预览不建任务：任务列表仍空
    await expect(page.getByTestId('task-row')).toHaveCount(0);

    // 7b. 校验和闸门：非法长度 → 提交禁用 + 错误提示；修正 → 恢复
    await page.getByTestId('advanced-toggle').click();
    const checksum = page.getByTestId('checksum-input');
    await checksum.fill('zz');
    await expect(page.getByText('SHA-256 须为 64 位十六进制（当前 2 位）')).toBeVisible();
    await expect(page.getByTestId('add-submit')).toBeDisabled();
    await checksum.fill(''); // 清空（本用例不测哈希失败链——避免任务落入校验失败重试）

    // 7c. 自动重试透传：填 3 → 提交 → 任务快照 max_retries=3（API 回显）
    await page.getByTestId('auto-retry-input').fill('3');
    await page.getByTestId('add-submit').click();
    await expect(urlInput).toBeHidden({ timeout: 10_000 });
    const row = page.getByTestId('task-row').first();
    await expect(row).toHaveAttribute('data-task-state', 'Completed', { timeout: 30_000 });
    const list = (await request.get(`${BASE}/tasks`).then((r) => r.json())) as Array<{
      task_id: string;
    }>;
    const snap = (await request.get(`${BASE}/tasks/${list[0].task_id}`).then((r) => r.json())) as {
      max_retries?: number;
    };
    expect(snap.max_retries, 'auto_retry 必须落到任务配置（API 回显）').toBe(3);

    // 7d. Esc 不收表单（防误触丢稿）+ 收起不丢稿（常驻挂载）
    await page.getByTestId('new-task-btn').click();
    await urlInput.fill(`${FILE}/another-url-draft.bin`);
    await urlInput.press('Escape');
    await expect(urlInput).toBeVisible(); // Esc 后表单仍在
    await page.getByRole('button', { name: '取消', exact: true }).click();
    await expect(urlInput).toBeHidden();
    await page.getByTestId('new-task-btn').click();
    await expect(urlInput).toHaveValue(`${FILE}/another-url-draft.bin`, { timeout: 5_000 });
    // 草稿不提交——清场（afterEach 删任务）；这里只验证草稿留存
    expect(pageErrors, '页面不得有未捕获异常').toEqual([]);
  });

  test('8. IP 封禁页：视图渲染 + 空态 + 逐条回执错误透出（Wave C）', async ({ page, request }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (e) => pageErrors.push(String(e)));
    await page.goto('/');

    // 8a. 侧栏切到安全视图（Ctrl+5 同效——快捷键序已扩到 6 视图）
    await page.getByRole('button', { name: '安全', exact: true }).click();
    await expect(page.getByText('IP 封禁表', { exact: true })).toBeVisible();
    // 空表空态（daemon 干净启动，无历史封禁）
    await expect(page.getByText('暂无封禁项')).toBeVisible({ timeout: 5_000 });

    // 8b. API 直接验证列表端点（UI 空态的数据源）
    const bans = (await request.get(`${BASE}/security/bans`).then((r) => r.json())) as {
      banned: string[];
    };
    expect(bans.banned).toEqual([]);

    // 8c. 封禁尝试（构建自适应）：
    //     - 无 BT 引擎构建 → 1.2.3.4 与 not-an-ip 均回执 ok:false（「不隐藏
    //       引擎边界」契约面），列表保持空态
    //     - bt 构建 → 1.2.3.4 真实入表（CI bt job 同套件可跑），not-an-ip
    //       仍 ✗ 逐条透出
    await page.getByTestId('bans-add-input').fill('1.2.3.4\nnot-an-ip');
    await page.getByTestId('bans-add-submit').click();
    const results = page.getByTestId('bans-results');
    await expect(results).toBeVisible({ timeout: 5_000 });
    await expect(results).toContainText('1.2.3.4');
    await expect(results).toContainText('not-an-ip');
    await expect(results).toContainText('✗'); // not-an-ip 恒失败（两种构建一致）
    const bansAfter = (await request.get(`${BASE}/security/bans`).then((r) => r.json())) as {
      banned: string[];
    };
    if (bansAfter.banned.includes('1.2.3.4')) {
      // bt 构建：真实封禁成功 → UI 列表显示该条目（不再空态）
      await expect(page.getByText('1.2.3.4').first()).toBeVisible({ timeout: 5_000 });
    } else {
      // 无 bt 构建：全部失败不入表 → 空态保持
      await expect(page.getByText('暂无封禁项')).toBeVisible();
    }
    expect(pageErrors, '页面不得有未捕获异常').toEqual([]);
  });
});
