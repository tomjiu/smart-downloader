import type { NextConfig } from "next";

// 静态导出（S2 内嵌）：产物 `ui/out/` 由 daemon `--ui-dir` 直接服务，
// 也作为 Tauri 桌面壳的资源打包。同源 `/api` 由反向代理/daemon 提供——
// 开发态用 next dev + 环境变量 NEXT_PUBLIC_DAEMON 指向后端。
const nextConfig: NextConfig = {
  output: "export",
  images: { unoptimized: true },
  trailingSlash: true,
};

export default nextConfig;
