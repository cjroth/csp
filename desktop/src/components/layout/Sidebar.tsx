import { Boxes, FolderGit2, Settings, Sparkles, Wand2 } from "lucide-react";
import { NavLink } from "react-router";
import { StatusBadge } from "@/components/layout/StatusDot";
import { useEngine } from "@/hooks/useEngine";
import { api } from "@/lib/api";
import { shortFp } from "@/lib/format";
import { cn } from "@/lib/utils";

function navClass({ isActive }: { isActive: boolean }) {
  return cn(
    "group relative flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition-all",
    isActive
      ? "bg-sidebar-accent text-sidebar-accent-foreground"
      : "text-muted-foreground hover:bg-sidebar-accent/50 hover:text-foreground",
  );
}

function ActiveBar({ isActive }: { isActive: boolean }) {
  return (
    <span
      className={cn(
        "absolute left-0 top-1/2 h-5 w-[3px] -translate-y-1/2 rounded-r-full bg-primary transition-all",
        isActive ? "opacity-100" : "opacity-0",
      )}
    />
  );
}

export function Sidebar() {
  const { aggregate, identity } = useEngine();

  return (
    <aside className="flex h-full w-64 shrink-0 flex-col border-r border-sidebar-border bg-sidebar">
      <div className="flex items-center gap-3 px-5 py-5">
        <div className="flex h-9 w-9 items-center justify-center rounded-xl bg-gradient-to-br from-violet-500 to-indigo-600 shadow-lg shadow-indigo-600/30">
          <Boxes className="h-5 w-5 text-white" />
        </div>
        <div className="leading-tight">
          <div className="text-sm font-semibold tracking-tight">Context</div>
          <div className="text-[11px] text-muted-foreground">Desktop sync</div>
        </div>
      </div>

      <nav className="flex flex-1 flex-col gap-1 px-3">
        <NavLink to="/" end className={navClass}>
          {({ isActive }) => (
            <>
              <ActiveBar isActive={isActive} />
              <FolderGit2 className="h-4 w-4" />
              Folders
            </>
          )}
        </NavLink>
        <NavLink to="/settings" className={navClass}>
          {({ isActive }) => (
            <>
              <ActiveBar isActive={isActive} />
              <Settings className="h-4 w-4" />
              Settings
            </>
          )}
        </NavLink>

        <div className="mt-auto mb-3 rounded-xl border border-sidebar-border bg-card/40 p-3">
          <div className="mb-2 flex items-center gap-1.5 text-[11px] font-medium uppercase tracking-wider text-muted-foreground">
            <Wand2 className="h-3 w-3" />
            Simulate
          </div>
          <div className="flex gap-2">
            <button
              type="button"
              onClick={() => void api.devTriggerTofu()}
              className="flex-1 rounded-md border border-border bg-background/40 px-2 py-1.5 text-xs font-medium text-muted-foreground transition-colors hover:border-primary/40 hover:text-foreground"
            >
              TOFU
            </button>
            <button
              type="button"
              onClick={() => void api.devTriggerSuperseded()}
              className="flex-1 rounded-md border border-border bg-background/40 px-2 py-1.5 text-xs font-medium text-muted-foreground transition-colors hover:border-primary/40 hover:text-foreground"
            >
              Edit
            </button>
          </div>
        </div>
      </nav>

      <div className="border-t border-sidebar-border p-4">
        <div className="flex items-center justify-between gap-2">
          <StatusBadge state={aggregate?.state ?? "idle"} />
          <span className="text-xs text-muted-foreground">
            {aggregate
              ? `${aggregate.vaultCount} folder${aggregate.vaultCount === 1 ? "" : "s"}`
              : "…"}
          </span>
        </div>
        <div
          className="mt-3 flex items-center gap-2 truncate font-mono text-[11px] text-muted-foreground"
          title={identity?.fingerprint}
        >
          <Sparkles className="h-3 w-3 shrink-0 text-primary/70" />
          {identity ? shortFp(identity.fingerprint) : "loading identity…"}
        </div>
      </div>
    </aside>
  );
}
