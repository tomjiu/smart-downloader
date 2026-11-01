/**
 * 插件页 e2e（#213-4：迅雷/夸克无入口 → 侧栏「插件」视图）。
 *
 * 覆盖面（真实 daemon，默认配置 = 零注册 Provider / 总开关关）：
 *   1. 侧栏导航：「插件」按钮存在 → 点击进入 → 工具栏标题 + 视图根
 *      testid（plugins-view）；
 *   2. 已知插件目录恒渲染：迅雷云盘 / 夸克网盘卡（未注册态徽章）——
 *      这是「无入口」问题的核心回归锚点（未启用也不许消失）；
 *   3. 配置快照语义：总开关未启用徽章（GET /config 消费回归）；
 *   4. 登录引导：迅雷 CLI 命令展示 + 复制按钮存在（不断言剪贴板——
 *      headless 权限环境差异，复制交互由组件 catch 兜底）；
 *   5. 空态说明：零注册 Provider 时的引导文案；
 *   6. 视图持久化：插件页刷新后保持（dl.view 恢复链路对新增视图生效）。
 */
import { test, expect } from '@playwright/test';

test.describe.serial('插件页（#213-4：Provider 目录 + 登录引导）', () => {
  test.afterEach(async ({ request }) => {
    // 本页只读（无任务操作），清场仅防未来用例追加任务后互相污染
    const res = await request.get(`${process.env.E2E_BASE_URL!}/tasks`);
    if (res.ok()) {
      const tasks = (await res.json()) as Array<{ task_id: string }>;
      for (const t of tasks) {
        await request.delete(`${process.env.E2E_BASE_URL!}/tasks/${t.task_id}?delete_data=true`).catch(() => {});
      }
    }
  });

  test('侧栏进入插件页：标题 + 已知插件目录恒渲染（未注册态）', async ({ page }) => {
    await page.goto('/');

    // 侧栏「插件」导航（折叠态下也有 title 提示，按钮恒在）
    await page.getByRole('button', { name: '插件', exact: true }).click();

    // 视图根 + 工具栏标题
    await expect(page.getByTestId('plugins-view')).toBeVisible();
    await expect(page.getByRole('heading', { name: '插件' })).toBeVisible();

    // 已知插件目录：未注册也不消失（「迅雷无入口」回归锚点）
    await expect(page.getByTestId('plugin-card-xunlei')).toBeVisible();
    await expect(page.getByTestId('plugin-card-xunlei')).toContainText('迅雷云盘');
    await expect(page.getByTestId('plugin-card-quark')).toBeVisible();
    await expect(page.getByTestId('plugin-card-quark')).toContainText('夸克网盘');

    // 默认 daemon：零注册 → 两卡均为「未注册」徽章
    await expect(page.getByTestId('plugin-card-xunlei').getByText('未注册')).toBeVisible();
    await expect(page.getByTestId('plugin-card-quark').getByText('未注册')).toBeVisible();
  });

  test('配置快照语义：总开关未启用徽章 + 空态引导文案', async ({ page }) => {
    await page.goto('/');
    await page.getByRole('button', { name: '插件', exact: true }).click();
    await expect(page.getByTestId('plugins-view')).toBeVisible();

    // GET /config 消费回归：默认 [provider] enabled=false → 总开关未启用
    await expect(page.getByText('总开关未启用')).toBeVisible({ timeout: 10_000 });

    // 零注册空态说明（默认 daemon providers=[]）
    await expect(page.getByText('当前未注册任何 Provider')).toBeVisible();
  });

  test('迅雷登录引导：CLI 命令展示 + 复制按钮 + 参数说明', async ({ page }) => {
    await page.goto('/');
    await page.getByRole('button', { name: '插件', exact: true }).click();
    const xl = page.getByTestId('plugin-card-xunlei');
    await expect(xl).toBeVisible();

    // 命令代码块（dl-code）+ 复制按钮（不断言剪贴板写入成功）
    await expect(xl.locator('.dl-code')).toContainText('smart-dl-daemon xunlei-login --page');
    await expect(xl.getByRole('button', { name: '复制登录命令' })).toBeVisible();

    // 参数说明关键项：token 路径 / 档位（当前档位来自 /config）
    await expect(xl.getByText('xunlei_auth.json')).toBeVisible();
    await expect(xl.getByText(/档位|tier/)).toBeVisible();
  });

  test('视图持久化：插件页刷新后保持（dl.view 恢复链路）', async ({ page }) => {
    await page.goto('/');
    await page.getByRole('button', { name: '插件', exact: true }).click();
    await expect(page.getByTestId('plugins-view')).toBeVisible();

    await page.reload();
    // 恢复链路：VIEW_KEYS 校验含 plugins → 恢复成功
    await expect(page.getByTestId('plugins-view')).toBeVisible();
  });
});
