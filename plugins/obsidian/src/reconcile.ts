// Startup convergence between the Obsidian-side vault content and the
// CSP-side thin-node working tree. Pure planning function — minimal
// interfaces, no Obsidian/SDK runtime deps, fully unit-testable.
//
// With the real CSP SDK this is largely subsumed by the engine: first
// attach and every reconnect run frontier-set anti-entropy catch-up (CSP
// spec.md §6.4) and §5.6 materialization. The host only adapts Obsidian's
// vault to the SDK's file surface and renders progress. This planner is
// retained because the in-memory mock has no frontier anti-entropy, and it
// keeps the first-attach pass deterministic and testable either way.

export interface ObsidianFileSummary {
  path: string;
  readText(): Promise<string>;
}

export interface SdkFileSummary {
  path: string;
  /** True when the engine has a tombstone for this path. */
  deleted: boolean;
  /** Only called when `deleted` is false. */
  readText(): Promise<string>;
}

export interface ReconcileInputs {
  obsidianFiles: readonly ObsidianFileSummary[];
  sdkFiles: readonly SdkFileSummary[];
  /** Scope filter — paths that fail this are excluded from both sides. */
  filter: (path: string) => boolean;
}

export interface PlannedSdkWrite {
  path: string;
  content: string;
}

export interface PlannedObsidianWrite {
  path: string;
  content: string;
  /** True when the file does not yet exist locally and must be created. */
  create: boolean;
}

export interface ReconcilePlan {
  pushToSdk: PlannedSdkWrite[];
  applyToObsidian: PlannedObsidianWrite[];
  deleteInObsidian: string[];
}

/**
 * Compute the work to make Obsidian and the engine byte-equal, given the
 * present state of both sides. Per path in the union:
 *
 *  - both alive, equal bytes  → no-op
 *  - obsidian-only            → push to engine
 *  - both alive, differ       → push obsidian's content (the engine folds
 *                                it against whatever the peer has — CSP §5)
 *  - engine-only, alive       → write to obsidian
 *  - engine tombstone + obs   → delete in obsidian
 */
export async function planReconcile(inputs: ReconcileInputs): Promise<ReconcilePlan> {
  const plan: ReconcilePlan = {
    pushToSdk: [],
    applyToObsidian: [],
    deleteInObsidian: [],
  };

  const obs = new Map<string, ObsidianFileSummary>();
  for (const f of inputs.obsidianFiles) {
    if (inputs.filter(f.path)) obs.set(f.path, f);
  }

  const sdk = new Map<string, SdkFileSummary>();
  for (const f of inputs.sdkFiles) {
    if (inputs.filter(f.path)) sdk.set(f.path, f);
  }

  const allPaths = new Set<string>();
  for (const k of obs.keys()) allPaths.add(k);
  for (const k of sdk.keys()) allPaths.add(k);

  for (const path of allPaths) {
    const o = obs.get(path);
    const s = sdk.get(path);

    if (o && s && s.deleted) {
      plan.deleteInObsidian.push(path);
      continue;
    }

    if (o && s && !s.deleted) {
      const oContent = await o.readText();
      const sContent = await s.readText();
      if (oContent !== sContent) {
        plan.pushToSdk.push({ path, content: oContent });
      }
      continue;
    }

    if (o && !s) {
      const content = await o.readText();
      plan.pushToSdk.push({ path, content });
      continue;
    }

    if (!o && s && !s.deleted) {
      const content = await s.readText();
      plan.applyToObsidian.push({ path, content, create: true });
    }
    // Else: !o && (no s OR s.deleted) — nothing to do.
  }

  return plan;
}
