"use client";
/// #213-1 桌面壳自绘标题栏（Windows/Linux 无装饰窗口）。
///
/// 渲染条件（唯一开关）：html[data-shell="frameless"]——由 layout.tsx 的
/// 启动脚本首帧同步判定（__TAURI__ 全局存在 && 非 macOS）。三种不渲染：
/// ① 浏览器（daemon 直接服务 UI，无 __TAURI__）；② macOS 壳（Rust 侧
/// 保留原生装饰，红绿灯/圆角/阴影是平台惯例）；③ 旗标在但 __TAURI__
/// window API 意外缺失（防御，不渲染交互件）。
///
/// 交互（走 withGlobalTauri 注入的 IPC，权限见 capabilities/shell-ui.json）：
/// - 拖拽/双击最大化：data-tauri-drag-region（Tauri 核心在 mousedown 时
///   识别该属性调用 start_dragging / internal_toggle_maximize）
/// - 最小化/最大化还原/关闭：getCurrentWindow() 对应方法（close 与原生
///   标题栏关闭同路——Rust 侧 CloseRequested 统一回收 daemon sidecar）
///
/// 视觉：遵循全局克制原则——无图标无标题（#213 用户要求去除左上角标识），
/// --bg-2 下沉层 + 1px --line 分界，与侧栏/状态栏同层；关闭钮 hover 走
/// Windows 惯例红。z-index 130 置于抽屉(91)/确认框(95)/toast(120) 之上：
/// 壳级 chrome 恒可见恒可点（抽屉打开时不锁死窗口控制，VS Code 同款）。
import { useEffect, useRef, useState } from "react";

type ShellWindow = {
  minimize: () => Promise<void>;
  toggleMaximize: () => Promise<void>;
  close: () => Promise<void>;
  isMaximized: () => Promise<boolean>;
  onResized: (cb: () => void) => Promise<() => void>;
};

type TauriGlobal = {
  window?: { getCurrentWindow?: () => ShellWindow };
};

function shellWindow(): ShellWindow | null {
  if (typeof window === "undefined") return null;
  try {
    const t = (window as unknown as { __TAURI__?: TauriGlobal }).__TAURI__;
    return t?.window?.getCurrentWindow?.() ?? null;
  } catch {
    return null;
  }
}

export default function TitleBar() {
  const [show, setShow] = useState(false);
  const [maximized, setMaximized] = useState(false);
  const winRef = useRef<ShellWindow | null>(null);

  useEffect(() => {
    if (document.documentElement.getAttribute("data-shell") !== "frameless") {
      return;
    }
    const w = shellWindow();
    if (!w) return;
    winRef.current = w;
    setShow(true);
    let alive = true;
    let unlisten: (() => void) | null = null;
    const refresh = async () => {
      try {
        const m = await w.isMaximized();
        if (alive) setMaximized(m);
      } catch {
        /* IPC 失败保持现状（capability 匹配面已覆盖回环全端口） */
      }
    };
    refresh();
    w.onResized(refresh)
      .then((un) => {
        if (alive) unlisten = un;
        else un();
      })
      .catch(() => {});
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  if (!show) return null;
  const w = winRef.current;

  const onToggleMax = async () => {
    if (!w) return;
    try {
      await w.toggleMaximize();
      // 乐观刷新（onResized 事件兜底）
      setMaximized(await w.isMaximized());
    } catch {
      /* 同上：静默 */
    }
  };

  return (
    <div className="dl-titlebar" data-tauri-drag-region>
      <div className="dl-titlebar-drag" data-tauri-drag-region />
      <button
        type="button"
        className="dl-titlebar-btn"
        aria-label="最小化"
        title="最小化"
        onClick={() => void w?.minimize().catch(() => {})}
      >
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <path d="M1 5h8" stroke="currentColor" strokeWidth="1" />
        </svg>
      </button>
      <button
        type="button"
        className="dl-titlebar-btn"
        aria-label={maximized ? "还原" : "最大化"}
        title={maximized ? "还原" : "最大化"}
        onClick={() => void onToggleMax()}
      >
        {maximized ? (
          /* 还原：前窗 + 后窗露一角 */
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <path d="M3.5 1.5h5v5" fill="none" stroke="currentColor" strokeWidth="1" />
            <rect x="1.5" y="3.5" width="5" height="5" fill="none" stroke="currentColor" strokeWidth="1" />
          </svg>
        ) : (
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
            <rect x="1.5" y="1.5" width="7" height="7" fill="none" stroke="currentColor" strokeWidth="1" />
          </svg>
        )}
      </button>
      <button
        type="button"
        className="dl-titlebar-btn dl-titlebar-btn--close"
        aria-label="关闭"
        title="关闭"
        onClick={() => void w?.close().catch(() => {})}
      >
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
          <path d="M1.5 1.5l7 7M8.5 1.5l-7 7" stroke="currentColor" strokeWidth="1" />
        </svg>
      </button>
    </div>
  );
}
