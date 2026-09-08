import type { Metadata, Viewport } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Smart Downloader",
  description: "多引擎下载器（HTTP/FTP/BT/HLS/Metalink）",
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="zh-CN" data-theme="forest-dark" suppressHydrationWarning>
      <head>
        <link rel="stylesheet" href="/qoder-ui/qoder-ui.min.css" />
        {/* 审计修复（40-e P1-7）：主题启动预置——旧实现 data-theme 硬编码
            forest-dark，dl.theme 只在设置页挂载时读取 → 每次启动/刷新都渲染
            暗色主题，直到点开设置页才恢复所选主题。内联脚本在任何渲染前
            应用持久化主题（与 next-themes 同模式；静态字符串无注入面）。 */}
        <script
          dangerouslySetInnerHTML={{
            __html:
              'try{var t=localStorage.getItem("dl.theme");if(t)document.documentElement.setAttribute("data-theme",t);}catch(e){}',
          }}
        />
      </head>
      <body>{children}</body>
    </html>
  );
}
