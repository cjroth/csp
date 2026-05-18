// Small display helpers. Cosmetic only — never reinterprets engine state.

export function relativeTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return "—";
  const diff = Date.now() - t;
  const s = Math.round(diff / 1000);
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.round(h / 24);
  if (d < 7) return `${d}d ago`;
  return new Date(iso).toLocaleDateString();
}

/** Short ed25519 fingerprint, e.g. "SHA256:ax9Q…I2oP". */
export function shortFp(fp: string): string {
  const body = fp.replace(/^SHA256:/, "");
  if (body.length <= 12) return fp;
  return `SHA256:${body.slice(0, 6)}…${body.slice(-4)}`;
}
