import type { Metadata, Viewport } from "next";
import "./tokens.css";
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
    // Issue #115 Phase 1：默认 nord-dark（规范 §6.2）。
    // 主题启动预置（审计 40-e P1-7 模式保留）：无 localStorage 时跟随
    // prefers-color-scheme（nord-dark / nord-light），在任何渲染前应用
    //（静态字符串无注入面）。
    <html lang="zh-CN" data-theme="nord-dark" suppressHydrationWarning>
      <head>
        <script
          dangerouslySetInnerHTML={{
            __html: `try{var t=localStorage.getItem("dl.theme");if(!t){t=matchMedia("(prefers-color-scheme: light)").matches?"nord-light":"nord-dark";}if(t)document.documentElement.setAttribute("data-theme",t);}catch(e){}`,
          }}
        />
      </head>
      <body>{children}</body>
    </html>
  );
}
