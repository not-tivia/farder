import { createContext, useContext, useReducer, useRef, useEffect, useCallback, ReactNode, MutableRefObject } from "react";
import { toast } from "../lib/toast";
import { getFloatAnchor } from "../lib/floatAnchor";

export const MAX_PLAYERS = 4;
const BASE_Z = 300; // above ScreenShareStage (z-index: 200)

export type PlayerKind = "video" | "iframe";
export type PlayerVisualState = "docked" | "floating" | "minimized";

export interface MediaPlayerInfo {
  id: string;
  kind: PlayerKind;
  src: string;
  title: string;
  hostId: string | null;
  state: PlayerVisualState;
  autoFloated: boolean;
  pos: { x: number; y: number };
  size: { w: number; h: number };
  opacity: number;
  z: number;
}

export interface OpenPlayerInput { kind: PlayerKind; src: string; hostId: string; title?: string; float?: boolean }
export type PlayerPatch = Partial<Pick<MediaPlayerInfo, "pos" | "size" | "opacity">>;

interface State { players: MediaPlayerInfo[]; nextZ: number; nextId: number }

type Action =
  | { type: "open"; input: OpenPlayerInput; anchor: { x: number; y: number; w: number; h: number } }
  | { type: "close"; id: string }
  | { type: "focus"; id: string }
  | { type: "update"; id: string; patch: PlayerPatch }
  | { type: "setState"; id: string; state: PlayerVisualState; autoFloated?: boolean }
  | { type: "hostVisible"; hostId: string; visible: boolean }
  | { type: "orphan"; hostId: string };

export const initialState: State = { players: [], nextZ: BASE_Z, nextId: 1 };

/**
 * Pure reducer — the testable core. No side effects (the over-cap toast lives in
 * the provider). Cap + dedupe are enforced here as a backstop.
 *
 * Test-notes (verified by inspection):
 *   - open (float=false) into empty → 1 player, id "mp-1", state "docked", z 300, nextId 2
 *   - open float=true               → state "floating", pos/size from anchor
 *   - open dup (same kind+src+hostId)→ no new player; existing focused (top z)
 *   - open 5th distinct             → unchanged (cap 4)
 *   - close id                      → removed
 *   - focus id                      → top z
 *   - update {pos}                  → only that player's pos
 *   - setState id "floating"        → that player floating (autoFloated as given, default false)
 *   - hostVisible(host,false) when docked → that player floating + autoFloated=true
 *   - hostVisible(host,true) when floating&autoFloated → docked + autoFloated=false
 *   - hostVisible(host,true) when floating&!autoFloated (popped out) → unchanged
 *   - orphan(host) → that player's hostId=null, state floating (if was docked), autoFloated=false
 */
export function mediaPlayersReducer(state: State, action: Action): State {
  switch (action.type) {
    case "open": {
      const { kind, src, hostId } = action.input;
      const existing = state.players.find((p) => p.kind === kind && p.src === src && p.hostId === hostId);
      if (existing) {
        return { ...state, nextZ: state.nextZ + 1, players: state.players.map((p) => p.id === existing.id ? { ...p, z: state.nextZ } : p) };
      }
      if (state.players.length >= MAX_PLAYERS) return state;
      const floating = !!action.input.float;
      const n = state.players.length;
      const a = action.anchor;
      const p: MediaPlayerInfo = {
        id: `mp-${state.nextId}`,
        kind, src,
        title: action.input.title ?? (kind === "video" ? "Video" : "Player"),
        hostId,
        state: floating ? "floating" : "docked",
        autoFloated: false,
        pos: { x: a.x + ((n * 28) % 140), y: a.y + ((n * 28) % 140) },
        size: { w: a.w, h: a.h },
        opacity: 1,
        z: state.nextZ,
      };
      return { players: [...state.players, p], nextZ: state.nextZ + 1, nextId: state.nextId + 1 };
    }
    case "close":
      return { ...state, players: state.players.filter((p) => p.id !== action.id) };
    case "focus":
      return { ...state, nextZ: state.nextZ + 1, players: state.players.map((p) => p.id === action.id ? { ...p, z: state.nextZ } : p) };
    case "update":
      return { ...state, players: state.players.map((p) => p.id === action.id ? { ...p, ...action.patch } : p) };
    case "setState":
      return { ...state, players: state.players.map((p) => p.id === action.id ? { ...p, state: action.state, autoFloated: action.autoFloated ?? false } : p) };
    case "hostVisible":
      return {
        ...state,
        players: state.players.map((p) => {
          if (p.hostId !== action.hostId || p.state === "minimized") return p;
          if (!action.visible && p.state === "docked") return { ...p, state: "floating", autoFloated: true };
          if (action.visible && p.state === "floating" && p.autoFloated) return { ...p, state: "docked", autoFloated: false };
          return p;
        }),
      };
    case "orphan":
      return {
        ...state,
        players: state.players.map((p) => p.hostId === action.hostId
          ? { ...p, hostId: null, autoFloated: false, state: p.state === "docked" ? "floating" : p.state }
          : p),
      };
    default:
      return state;
  }
}

interface CtxValue {
  players: MediaPlayerInfo[];
  hosts: MutableRefObject<Map<string, HTMLElement>>;
  openPlayer: (input: OpenPlayerInput) => void;
  closePlayer: (id: string) => void;
  focusPlayer: (id: string) => void;
  updatePlayer: (id: string, patch: PlayerPatch) => void;
  setPlayerState: (id: string, state: PlayerVisualState) => void;
  registerHost: (hostId: string, el: HTMLElement) => void;
  unregisterHost: (hostId: string) => void;
  setHostVisible: (hostId: string, visible: boolean) => void;
}

const Ctx = createContext<CtxValue | null>(null);

export function MediaPlayersProvider({ children }: { children: ReactNode }) {
  const [state, dispatch] = useReducer(mediaPlayersReducer, initialState);
  const stateRef = useRef(state);
  useEffect(() => { stateRef.current = state; }, [state]);
  const hosts = useRef<Map<string, HTMLElement>>(new Map());

  const openPlayer = useCallback((input: OpenPlayerInput) => {
    const s = stateRef.current;
    const dup = s.players.some((p) => p.kind === input.kind && p.src === input.src && p.hostId === input.hostId);
    if (!dup && s.players.length >= MAX_PLAYERS) { toast.info("Close a player to open another"); return; }
    dispatch({ type: "open", input, anchor: getFloatAnchor() });
  }, []);
  const closePlayer = useCallback((id: string) => dispatch({ type: "close", id }), []);
  const focusPlayer = useCallback((id: string) => dispatch({ type: "focus", id }), []);
  const updatePlayer = useCallback((id: string, patch: PlayerPatch) => dispatch({ type: "update", id, patch }), []);
  const setPlayerState = useCallback((id: string, st: PlayerVisualState) => dispatch({ type: "setState", id, state: st }), []);
  const registerHost = useCallback((hostId: string, el: HTMLElement) => { hosts.current.set(hostId, el); }, []);
  const unregisterHost = useCallback((hostId: string) => { hosts.current.delete(hostId); dispatch({ type: "orphan", hostId }); }, []);
  const setHostVisible = useCallback((hostId: string, visible: boolean) => dispatch({ type: "hostVisible", hostId, visible }), []);

  return (
    <Ctx.Provider value={{ players: state.players, hosts, openPlayer, closePlayer, focusPlayer, updatePlayer, setPlayerState, registerHost, unregisterHost, setHostVisible }}>
      {children}
    </Ctx.Provider>
  );
}

export function useMediaPlayers(): CtxValue {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useMediaPlayers must be used within a MediaPlayersProvider");
  return ctx;
}
