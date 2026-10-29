/**
 * #213-1 自绘标题栏 e2e：桌面壳环境模拟（__TAURI__ 全局注入）。
 *
 * 真实壳行为无法在浏览器 e2e 里复现（无 tauri webview），但可注入
 * 同形 mock 验证 UI 侧全部逻辑：探测门（data-shell 旗标首帧同步）/
 * 渲染与布局让位 / 三按钮 IPC 调用 / 最大化态图标切换 / macOS 与
 * 浏览器两态不渲染。真实壳编译由 fork desktop.yml 覆盖（runbook 纪律）。
 *
 *   TB-1 壳环境：TitleBar 渲染 + html 旗标 + dl-root 顶部让位（padding）
 *       + 拖拽区 data-tauri-drag-region 属性（Tauri 核心识别约定）
 *   TB-2 窗口控制：最小化/关闭按钮 → mock IPC 调用铁证；最大化按钮 →
 *       toggleMaximize + isMaximized 联动 → aria-label 最大化↔还原切换
 *   TB-3 macOS 壳：__TAURI__ 在但 UA 为 Mac → 原生装饰路径，不渲染
 *       不占位（Rust 侧 #[cfg] 同款判定）
 *   TB-4 浏览器：无 __TAURI__（其余 17 个 spec 的默认环境）→ 不渲染
 *       不占位——防「浏览器里出现幽灵标题栏」回归
 */
import { test, expect, type Page } from '@playwright/test';

/** 与真实 withGlobalTauri 注入同形的 mock（仅 UI 消费的字段）。 */
const TAURI_MOCK = `(() => {
  window.__tb_calls = [];
  window.__tb_max = false;
  window.__TAURI__ = {
    window: {
      getCurrentWindow: () => ({
        minimize: async () => { window.__tb_calls.push('minimize'); },
        toggleMaximize: async () => {
          window.__tb_calls.push('toggleMaximize');
          window.__tb_max = !window.__tb_max;
        },
        close: async () => { window.__tb_calls.push('close'); },
        isMaximized: async () => window.__tb_max,
        onResized: async (cb) => {
          window.__tb_onresized = cb;
          return () => {};
        },
      }),
    },
  };
})();`;

const MAC_UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15';

async function calls(page: Page): Promise<string[]> {
  return page.evaluate(() => (window as unknown as { __tb_calls?: string[] }).__tb_calls ?? []);
}

test.describe('#213-1 桌面壳自绘标题栏', () => {
  test('TB-1 壳环境：渲染 + 旗标 + 布局让位 + 拖拽区', async ({ page }) => {
    await page.addInitScript(TAURI_MOCK);
    await page.goto('/');
    // 首帧同步旗标（无闪跳的关键：CSS 让位不等 React）
    await expect(page.locator('html')).toHaveAttribute('data-shell', 'frameless');
    const bar = page.locator('.dl-titlebar');
    await expect(bar).toBeVisible();
    // 布局让位：dl-root 顶部 padding = 标题栏高（36px > 0）
    const pad = await page.evaluate(() =>
      parseFloat(getComputedStyle(document.querySelector('.dl-root')!).paddingTop),
    );
    expect(pad).toBeGreaterThan(0);
    // 拖拽区属性：Tauri 核心 mousedown 识别约定（根 + 拖拽 spacer 均带）
    await expect(bar).toHaveAttribute('data-tauri-drag-region');
    await expect(bar.locator('.dl-titlebar-drag')).toHaveAttribute('data-tauri-drag-region');
    // 三控件齐备
    await expect(bar.getByRole('button', { name: '最小化' })).toBeVisible();
    await expect(bar.getByRole('button', { name: '最大化' })).toBeVisible();
    await expect(bar.getByRole('button', { name: '关闭' })).toBeVisible();
    // 无图标无标题（#213 用户要求：去除左上角标识）
    await expect(bar.getByText('Smart Downloader')).toHaveCount(0);
  });

  test('TB-2 窗口控制：三按钮 IPC 调用 + 最大化态图标切换', async ({ page }) => {
    await page.addInitScript(TAURI_MOCK);
    await page.goto('/');
    // 作用域限定标题栏：全套运行时共享 daemon 的残留活动会触发 live toast，
    // 其「关闭提示」按钮与「关闭」构成子串撞名（strict mode violation 实证）
    const bar = page.locator('.dl-titlebar');
    await expect(bar).toBeVisible();

    await bar.getByRole('button', { name: '最小化' }).click();
    await expect.poll(() => calls(page)).toContainEqual('minimize');

    await bar.getByRole('button', { name: '关闭', exact: true }).click();
    await expect.poll(() => calls(page)).toContainEqual('close');

    // 最大化 → mock isMaximized 翻 true → 图标/aria-label 切「还原」
    await bar.getByRole('button', { name: '最大化' }).click();
    await expect.poll(() => calls(page)).toContainEqual('toggleMaximize');
    await expect(bar.getByRole('button', { name: '还原' })).toBeVisible();
    // 还原 → 翻回
    await bar.getByRole('button', { name: '还原' }).click();
    await expect(bar.getByRole('button', { name: '最大化' })).toBeVisible();
  });

  test('TB-3 macOS 壳：保留原生装饰——不渲染不占位', async ({ browser }) => {
    const ctx = await browser.newContext({ userAgent: MAC_UA });
    const page = await ctx.newPage();
    try {
      await page.addInitScript(TAURI_MOCK);
      await page.goto('/');
      await expect(page.locator('.dl-titlebar')).toHaveCount(0);
      await expect(page.locator('html')).not.toHaveAttribute('data-shell', 'frameless');
      const pad = await page.evaluate(() =>
        getComputedStyle(document.querySelector('.dl-root')!).paddingTop,
      );
      expect(pad).toBe('0px');
    } finally {
      await ctx.close();
    }
  });

  test('TB-4 浏览器（无 __TAURI__）：不渲染不占位', async ({ page }) => {
    await page.goto('/');
    await expect(page.locator('.dl-titlebar')).toHaveCount(0);
    await expect(page.locator('html')).not.toHaveAttribute('data-shell', 'frameless');
    const pad = await page.evaluate(() =>
      getComputedStyle(document.querySelector('.dl-root')!).paddingTop,
    );
    expect(pad).toBe('0px');
  });
});
