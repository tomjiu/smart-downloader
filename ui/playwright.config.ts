import { defineConfig } from '@playwright/test';

/**
 * e2e 配置：真实 daemon（--ui-dir 服务 ui/out 产物）+ 本地限流文件服务器。
 *
 * 运行前置：
 *   1. cd ui && bun run build            （产出 ui/out）
 *   2. cargo build -p smart-dl-daemon    （debug 即可）
 *   3. bunx playwright install chromium  （首次）
 * 运行：bun run test:e2e
 *
 * 串行执行：全部用例共享同一 daemon（真实状态累积），afterEach 清场。
 * 不进 CI（REALTEST-PLAN 的 CI 集成只含契约矩阵/并发 job；浏览器依赖重，
 * 本地/实机跑——与 scripts/manual/ 的手工实机脚本同一姿态）。
 */
export default defineConfig({
  testDir: './e2e',
  timeout: 90_000,
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  use: {
    baseURL: process.env.E2E_BASE_URL || 'http://127.0.0.1:8788',
    headless: true,
    trace: 'retain-on-failure',
    actionTimeout: 10_000,
  },
  globalSetup: './e2e/global-setup.ts',
  projects: [{ name: 'chromium', use: { browserName: 'chromium' } }],
});
