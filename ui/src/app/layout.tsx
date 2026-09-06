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
      </head>
      <body>{children}</body>
    </html>
  );
}
