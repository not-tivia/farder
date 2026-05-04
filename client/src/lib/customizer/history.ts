import type { RegionId, RegionState } from "./types";

export type RegionsMap = Map<RegionId, RegionState>;

export interface HistoryState {
  history: RegionsMap[];
  index: number;
}

/** Deep-clone a regions map so future mutations don't bleed into history snapshots. */
function cloneRegions(regions: RegionsMap): RegionsMap {
  const out = new Map<RegionId, RegionState>();
  for (const [k, v] of regions) {
    out.set(k, {
      bgColor: v.bgColor,
      bgImage: v.bgImage ? { path: v.bgImage.path, fit: v.bgImage.fit } : undefined,
      textColor: v.textColor,
    });
  }
  return out;
}

/** Initialize a history with the given starting regions snapshot. */
export function initHistory(initial: RegionsMap): HistoryState {
  return { history: [cloneRegions(initial)], index: 0 };
}

/** Push a new snapshot. Truncates anything after the current index (redo branch lost on new edit). */
export function pushSnapshot(state: HistoryState, next: RegionsMap): HistoryState {
  const truncated = state.history.slice(0, state.index + 1);
  truncated.push(cloneRegions(next));
  return { history: truncated, index: truncated.length - 1 };
}

/** Move back one step. Returns same state if already at index 0. */
export function undo(state: HistoryState): HistoryState {
  if (state.index <= 0) return state;
  return { history: state.history, index: state.index - 1 };
}

/** Move forward one step. Returns same state if already at the end. */
export function redo(state: HistoryState): HistoryState {
  if (state.index >= state.history.length - 1) return state;
  return { history: state.history, index: state.index + 1 };
}

/** Read the snapshot at the current index. Returns a clone (callers may mutate). */
export function current(state: HistoryState): RegionsMap {
  return cloneRegions(state.history[state.index]);
}

export function canUndo(state: HistoryState): boolean {
  return state.index > 0;
}

export function canRedo(state: HistoryState): boolean {
  return state.index < state.history.length - 1;
}
