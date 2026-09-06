/// 格式化助手（下载器 UI 口径）。

export function fmtBytes(n: number | undefined | null): string {
  if (n == null || !isFinite(n)) return "—";
  if (n < 1024) return `${n} B`;
  const units = ["KiB", "MiB", "GiB", "TiB", "PiB"];
  let v = n / 1024;
  for (const u of units) {
    if (v < 1024) return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${u}`;
    v /= 1024;
  }
  return `${v.toFixed(1)} EiB`;
}

export function fmtSpeed(bps: number | undefined | null): string {
  if (bps == null || !isFinite(bps) || bps <= 0) return "0 B/s";
  return `${fmtBytes(bps)}/s`;
}

export function fmtPct(done: number, total: number): string {
  if (!total) return "0%";
  const p = (done / total) * 100;
  return p >= 100 ? "100%" : `${p < 10 ? p.toFixed(1) : p.toFixed(1)}%`;
}

export function fmtEta(done: number, total: number, bps: number): string {
  if (bps <= 0 || total <= 0 || done >= total) return "—";
  const s = Math.round((total - done) / bps);
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`;
}

export function fmtTime(iso: string | undefined | null): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (isNaN(d.getTime())) return "—";
  return d.toLocaleString(undefined, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function fmtKb(kb: number | undefined | null): string {
  if (kb == null || !isFinite(kb)) return "—";
  return kb === 0 ? "不限" : `${kb} KiB/s`;
}
