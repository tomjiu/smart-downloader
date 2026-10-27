/**
 * 磁力元数据预览 e2e（体检 #182 R7：/bt/metadata 接入 NewTaskForm）。
 * 构建自适应（超合并验证 F2 修复）：/version features.bt 判别——
 *   - 非 bt 构建：/bt/metadata 立即 400「需 BT 引擎」→ 优雅报错路径；
 *   - bt 构建：真跑 swarm 抓取——死 infohash 于 timeout_s（UI 固定 30s）
 *     到期优雅失败，顺带端到端验证 timeout_s 契约遵从。
 * 成功路径（公网 swarm 抓取）已由 scripts/manual/ rt-13 在 API 层全量实证。
 * 契约：magnet 链接 → 「元数据预览」按钮（替代 http 直链的「预览」）；
 * 失败 → 错误条可见（勿用 getByRole('alert').first()——Next.js 的
 * __next-route-announcer__ 恒为 role=alert，会劫持泛化断言）；
 * http(s) 链接 → 原「预览」按钮不受影响。
 */
import { test, expect } from '@playwright/test';

const BASE = process.env.E2E_BASE_URL!;

test('磁力预览：按钮切换 + 优雅报错（构建自适应）+ http 探测不受影响', async ({
  page,
  request,
}) => {
  // 构建判别：/version 暴露编译特性（部署矩阵契约）
  const ver = (await (await request.get(`${BASE}/version`)).json()) as {
    features: { bt: boolean };
  };
  const btBuild = ver.features.bt;

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

  // 3) 点击 → 优雅报错（不崩、表单仍在、输入稿保留）
  await page.getByTestId('meta-btn').click();
  if (btBuild) {
    // bt 构建：死 infohash 无 tracker/DHT 命中 → timeout_s(30s) 到期失败。
    // 40s 窗口 = 30s 超时 + 余量；此断言同时验证超时契约端到端遵从。
    await expect(page.getByText(/元数据抓取失败.*超时/).first()).toBeVisible({
      timeout: 40_000,
    });
  } else {
    // 非 bt 构建：立即 400「需 BT 引擎」
    await expect(page.getByText(/元数据抓取失败/).first()).toBeVisible({ timeout: 15_000 });
  }
  // 输入稿保留（失败不清空）
  await expect(input).toHaveValue(/magnet:/);
});
