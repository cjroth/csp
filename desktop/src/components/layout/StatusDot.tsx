import type { SyncState } from "@/lib/api.types";
import { cn } from "@/lib/utils";

interface Meta {
  dot: string;
  glow: string;
  text: string;
  ring: string;
  label: string;
}

const META: Record<SyncState, Meta> = {
  idle: {
    dot: "bg-zinc-400",
    glow: "shadow-[0_0_0_3px_oklch(0.7_0.01_264/0.15)]",
    text: "text-zinc-300",
    ring: "bg-zinc-500/12 text-zinc-300 ring-zinc-400/20",
    label: "Idle",
  },
  syncing: {
    dot: "bg-sky-400",
    glow: "shadow-[0_0_10px_1px_oklch(0.7_0.13_215/0.7)]",
    text: "text-sky-300",
    ring: "bg-sky-500/12 text-sky-300 ring-sky-400/25",
    label: "Syncing",
  },
  synced: {
    dot: "bg-emerald-400",
    glow: "shadow-[0_0_10px_1px_oklch(0.72_0.16_150/0.6)]",
    text: "text-emerald-300",
    ring: "bg-emerald-500/12 text-emerald-300 ring-emerald-400/25",
    label: "Synced",
  },
  offline: {
    dot: "bg-amber-400",
    glow: "shadow-[0_0_10px_1px_oklch(0.78_0.15_75/0.55)]",
    text: "text-amber-300",
    ring: "bg-amber-500/12 text-amber-300 ring-amber-400/25",
    label: "Offline",
  },
  attention: {
    dot: "bg-red-400",
    glow: "shadow-[0_0_10px_1px_oklch(0.66_0.21_18/0.65)]",
    text: "text-red-300",
    ring: "bg-red-500/12 text-red-300 ring-red-400/25",
    label: "Needs attention",
  },
};

export function StatusDot({
  state,
  withLabel = false,
  className,
}: {
  state: SyncState;
  withLabel?: boolean;
  className?: string;
}) {
  const m = META[state];
  return (
    <span className={cn("inline-flex items-center gap-2", className)}>
      <span className="relative flex h-2.5 w-2.5 items-center justify-center">
        {state === "syncing" && (
          <span className={cn("absolute h-full w-full animate-ping rounded-full", m.dot)} />
        )}
        <span className={cn("relative h-2.5 w-2.5 rounded-full", m.dot, m.glow)} />
      </span>
      {withLabel && <span className={cn("text-sm font-medium", m.text)}>{m.label}</span>}
    </span>
  );
}

/** Pill: tinted background + ring + dot + label. */
export function StatusBadge({ state, className }: { state: SyncState; className?: string }) {
  const m = META[state];
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium ring-1 ring-inset",
        m.ring,
        className,
      )}
    >
      <span className={cn("h-1.5 w-1.5 rounded-full", m.dot, m.glow)} />
      {m.label}
    </span>
  );
}
