import { defineConfig } from '@playwright/test';

/**
 * R18 重启韧性独立套件配置：与主套件（e2e/）隔离——本套件的 spec 自带
 * daemon 生命周期（kill -9 + 重启），不能与共享 daemon 的主套件混跑。
 *
 * 运行前置：同主套件（ui/out 已构建 + daemon debug 二进制）。
 * 运行：cd ui && npx playwright test -c e2e-restart/playwright.config.ts
 */
export default defineConfig({
  testDir: '.',
  timeout: 120_000,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  use: {
    baseURL: 'http://127.0.0.1:8791',
    headless: true,
    trace: 'retain-on-failure',
    actionTimeout: 10_000,
  },
  projects: [{ name: 'chromium', use: { browserName: 'chromium' } }],
});
