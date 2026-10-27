/**
 * 磁力元数据预览 e2e（体检 #182 R7：/bt/metadata 接入 NewTaskForm）。
 * e2e daemon 默认构建无 bt feature → 覆盖优雅错误路径（明确报错不崩）；
 * 成功路径（公网 swarm 抓取）已由 scripts/manual/ rt-13 在 API 层全量实证。
 * 契约：magnet 链接 → 「元数据预览」按钮（替代 http 直链的「预览」）；
 * 失败 → role=alert 错误条可见；http(s) 链接 → 原「预览」按钮不受影响。
 */
import { test, expect } from '@playwright/test';

const BASE = process.env.E2E_BASE_URL!;

test('磁力预览：按钮切换 + 无 bt 引擎优雅报错 + http 探测不受影响', async ({ page }) => {
  await page.goto(BASE!);
  await page.getByTestId('new-task-btn').click();
  const input = page.getByTestId('add-url-input');

  // 1) http 链接 → 原「预览」（探测）按钮
  await input.fill('http://example.com/file.bin');
  await expect(page.getByTestId('probe-btn')).toBeVisible();
  await expect(page.getByTestId('meta-btn')).toHaveCount(0);

  // 2) magnet 链接 → 「元数据预览」按钮
  await input.fill('magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=test');
  await expect(page.getByTestId('meta-btn')).toBeVisible();
  await expect(page.getByTestId('probe-btn')).toHaveCount(0);

  // 3) 点击 → e2e daemon 无 bt feature → 优雅报错（不崩、表单仍在）
  await page.getByTestId('meta-btn').click();
  await expect(page.getByRole('alert').first()).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText(/元数据抓取失败/).first()).toBeVisible();
  // 输入稿保留（失败不清空）
  await expect(input).toHaveValue(/magnet:/);
});
