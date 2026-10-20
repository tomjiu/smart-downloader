"use client";
/// 内联 SVG 图标（Issue #115 Phase 1：旧字体图标库移除后的替代层）。
/// 24px viewBox / stroke 风格（Octicons 简化线稿），currentColor 继承文字色。
/// 规格：规范 §5.4 图标按钮 24×24。

export type IconName =
  | "arrow-down"
  | "arrow-up"
  | "check"
  | "close"
  | "cloud-download"
  | "file"
  | "file-directory"
  | "battery"
  | "globe"
  | "shield"
  | "zap"
  | "graph"
  | "layers"
  | "list-unordered"
  | "output"
  | "paintcan"
  | "pause"
  | "pie-chart"
  | "play"
  | "plus"
  | "radio-tower"
  | "save"
  | "server"
  | "settings-gear"
  | "speed"
  | "sync"
  | "trash"
  | "kebab-horizontal"
  | "pass"
  | "error"
  | "search"
  | "panel-left"
  | "chevron-left";

const PATHS: Record<IconName, React.ReactNode> = {
  "arrow-down": <path d="M12 4v14m0 0l-5-5m5 5l5-5" />,
  "arrow-up": <path d="M12 20V6m0 0l-5 5m5-5l5 5" />,
  battery: (
    <path d="M3 9V7h3l1.5-2h6L15 7h3v2M3 9v8h15V9M3 9h15M8 13h2m2 0h2" />
  ),
  check: <path d="M4 12.5l5 5L20 6.5" />,
  close: <path d="M6 6l12 12M18 6L6 18" />,
  "cloud-download": (
    <>
      <path d="M6.5 18a4.5 4.5 0 01-.4-8.98A6 6 0 0117.7 8.3 4.25 4.25 0 0117 18h-4" />
      <path d="M12 12v7m0 0l-2.5-2.5M12 19l2.5-2.5" />
    </>
  ),
  file: (
    <>
      <path d="M6 3h8l4 4v14H6V3z" />
      <path d="M14 3v4h4" />
    </>
  ),
  "file-directory": <path d="M3 6a1 1 0 011-1h5l2 2h9a1 1 0 011 1v11a1 1 0 01-1 1H4a1 1 0 01-1-1V6z" />,
  globe: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M3 12h18M12 3c2.5 2.5 3.5 5.5 3.5 9S14.5 18.5 12 21c-2.5-2.5-3.5-5.5-3.5-9S9.5 5.5 12 3z" />
    </>
  ),
  graph: <path d="M4 19V5m0 14h16M7 15l4-5 3 3 5-7" />,
  layers: <path d="M12 3l9 5-9 5-9-5 9-5zm9 9l-9 5-9-5m18 4l-9 5-9-5" />,
  "list-unordered": <path d="M8 6h12M8 12h12M8 18h12M4 6h.01M4 12h.01M4 18h.01" />,
  output: (
    <>
      <path d="M4 4h16v12H4V4z" />
      <path d="M8 20h8M12 16v4" />
    </>
  ),
  paintcan: (
    <>
      <path d="M6 9h12v6a5 5 0 01-5 5h-2a5 5 0 01-5-5V9z" />
      <path d="M6 6a3 3 0 013-3h6a3 3 0 013 3v3H6V6z" />
    </>
  ),
  pause: <path d="M8 5v14M16 5v14" />,
  "pie-chart": (
    <>
      <path d="M12 3a9 9 0 109 9h-9V3z" />
      <path d="M15 3.6A9 9 0 0120.4 9H15V3.6z" />
    </>
  ),
  play: <path d="M7 4.5l12 7.5-12 7.5v-15z" />,
  plus: <path d="M12 5v14M5 12h14" />,
  "radio-tower": (
    <>
      <path d="M12 11a1.5 1.5 0 100 3 1.5 1.5 0 000-3z" />
      <path d="M8.5 8.5a5 5 0 000 7M15.5 8.5a5 5 0 010 7M6 6a8.5 8.5 0 000 12M18 6a8.5 8.5 0 010 12" />
      <path d="M12 12L8 21m4-9l4 9" />
    </>
  ),
  save: (
    <>
      <path d="M5 3h11l5 5v13H5V3z" />
      <path d="M8 3v6h8V3M8 21v-8h8v8" />
    </>
  ),
  server: (
    <>
      <rect x="4" y="4" width="16" height="6" rx="1" />
      <rect x="4" y="14" width="16" height="6" rx="1" />
      <path d="M8 7h.01M8 17h.01" />
    </>
  ),
  "settings-gear": (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 2v3m0 14v3M4.2 4.2l2.1 2.1m11.4 11.4l2.1 2.1M2 12h3m14 0h3M4.2 19.8l2.1-2.1M17.7 6.3l2.1-2.1" />
    </>
  ),
  shield: <path d="M12 3l8 3v6c0 4.4-3.4 7.9-8 9-4.6-1.1-8-4.6-8-9V6l8-3zM9 12l2 2 4-4" />,
  speed: (
    <>
      <path d="M4 16a8 8 0 1116 0" />
      <path d="M12 16l4-6" />
    </>
  ),
  sync: <path d="M20 11a8 8 0 10-2.3 5.7M20 5v6h-6" />,
  zap: <path d="M13 2L4 14h6l-1 8 9-12h-6l1-8z" />,
  trash: <path d="M4 7h16M9 7V4h6v3m-8 0l1 13h8l1-13M10 11v6m4-6v6" />,
  "kebab-horizontal": <path d="M6 12h.01M12 12h.01M18 12h.01" />,
  pass: <path d="M4 13l4 4L20 7" />,
  error: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v6m0 4h.01" />
    </>
  ),
  search: (
    <>
      <circle cx="10" cy="10" r="6" />
      <path d="M14.5 14.5L20 20" />
    </>
  ),
  /* 2026-10-19 布局重设计：侧栏折叠开关（panel）与方向指示 */
  "panel-left": (
    <>
      <rect x="3" y="4" width="18" height="16" rx="1" />
      <path d="M9 4v16" />
    </>
  ),
  "chevron-left": <path d="M14 6l-6 6 6 6" />,
};

export function Icon({
  name,
  size = 16,
  className,
  style,
}: {
  name: IconName;
  size?: number;
  className?: string;
  style?: React.CSSProperties;
}) {
  return (
    <svg
      className={className}
      style={style}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {PATHS[name]}
    </svg>
  );
}
