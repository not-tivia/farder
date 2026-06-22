import { createContext, useContext, useReducer, useRef, useEffect, useCallback, ReactNode } from "react";
import { toast } from "../lib/toast";

export const MAX_PIPS = 4;
const BASE_Z = 300; // above ScreenShareStage (z-index: 200)

export interface PipPaneState {
  id: string;
  mediaUrl: string;
  title: string;
  pos: { x: number; y: number };
  size: { w: number; h: number };
  opacity: number;
  minimized: boolean;
  z: number;
}

export type PipPatch = Partial<Pick<PipPaneState, "pos" | "size" | "opacity" | "minimized">>;
export interface PipOpenInput { mediaUrl: string; title?: string; mime?: string }

interface PipState { panes: PipPaneState[]; nextZ: number; nextId: number }

type PipAction =
  | { type: "open"; input: PipOpenInput }
  | { type: "close"; id: string }
  | { type: "focus"; id: string }
  | { type: "update"; id: string; patch: PipPatch };

export const initialPipState: PipState = { panes: [], nextZ: BASE_Z, nextId: 1 };

/**
 * Pure reducer — the testable core of the PiP manager. No side effects (the
 * over-cap toast lives in the provider). Dedupe + cap are enforced here so the
 * state is always correct even if the provider's pre-check is racy.
 *
 * Test-notes (manually verified by inspection):
 *   - open into empty state            → 1 pane, id "pip-1", z 300, nextZ 301, nextId 2
 *   - open a 2nd distinct mediaUrl     → 2 panes, 2nd z 301, cascade pos offset
 *   - open a mediaUrl already present  → no new pane; existing pane gets top z + minimized=false
 *   - open a 5th distinct mediaUrl     → state unchanged (cap of 4)
 *   - close an id                      → that pane removed; others untouched
 *   - focus an id                      → that pane gets nextZ; nextZ increments
 *   - update {opacity}                 → only that pane's opacity changes
 */
export function pipReducer(state: PipState, action: PipAction): PipState {
  switch (action.type) {
    case "open": {
      const existing = state.panes.find((p) => p.mediaUrl === action.input.mediaUrl);
      if (existing) {
        return {
          ...state,
          nextZ: state.nextZ + 1,
          panes: state.panes.map((p) =>
            p.id === existing.id ? { ...p, z: state.nextZ, minimized: false } : p,
          ),
        };
      }
      if (state.panes.length >= MAX_PIPS) return state; // cap (toast in provider)
      const n = state.panes.length;
      const pane: PipPaneState = {
        id: `pip-${state.nextId}`,
        mediaUrl: action.input.mediaUrl,
        title: action.input.title ?? "Video",
        pos: { x: 80 + ((n * 28) % 220), y: 80 + ((n * 28) % 220) },
        size: { w: 360, h: 240 },
        opacity: 1,
        minimized: false,
        z: state.nextZ,
      };
      return { panes: [...state.panes, pane], nextZ: state.nextZ + 1, nextId: state.nextId + 1 };
    }
    case "close":
      return { ...state, panes: state.panes.filter((p) => p.id !== action.id) };
    case "focus":
      return {
        ...state,
        nextZ: state.nextZ + 1,
        panes: state.panes.map((p) => (p.id === action.id ? { ...p, z: state.nextZ } : p)),
      };
    case "update":
      return {
        ...state,
        panes: state.panes.map((p) => (p.id === action.id ? { ...p, ...action.patch } : p)),
      };
    default:
      return state;
  }
}

interface PipContextValue {
  panes: PipPaneState[];
  openPip: (input: PipOpenInput) => void;
  closePip: (id: string) => void;
  focusPip: (id: string) => void;
  updatePip: (id: string, patch: PipPatch) => void;
}

const PipContext = createContext<PipContextValue | null>(null);

export function PipProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(pipReducer, initialPipState);
  // Ref mirrors the latest state so openPip can decide whether to toast WITHOUT
  // doing a side effect inside the reducer (which StrictMode double-invokes).
  const stateRef = useRef(state);
  useEffect(() => { stateRef.current = state; }, [state]);

  const openPip = useCallback((input: PipOpenInput) => {
    const s = stateRef.current;
    const isDup = s.panes.some((p) => p.mediaUrl === input.mediaUrl);
    if (!isDup && s.panes.length >= MAX_PIPS) {
      toast.info("Close a video to open another");
      return;
    }
    dispatch({ type: "open", input });
  }, []);

  const closePip = useCallback((id: string) => dispatch({ type: "close", id }), []);
  const focusPip = useCallback((id: string) => dispatch({ type: "focus", id }), []);
  const updatePip = useCallback((id: string, patch: PipPatch) => dispatch({ type: "update", id, patch }), []);

  return (
    <PipContext.Provider value={{ panes: state.panes, openPip, closePip, focusPip, updatePip }}>
      {children}
    </PipContext.Provider>
  );
}

export function usePip(): PipContextValue {
  const ctx = useContext(PipContext);
  if (!ctx) throw new Error("usePip must be used within a PipProvider");
  return ctx;
}
