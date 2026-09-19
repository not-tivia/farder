import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

/**
 * The window's ONLY title bar.
 *
 * Until now the app drew this one AND Windows drew its own above it, so Farder
 * appeared to run inside a second window called Farder. `decorations: false` in
 * `tauri.conf.json` removes the native one, which makes this bar responsible for
 * the three things the OS used to do:
 *
 *  - **moving the window** — `data-tauri-drag-region`, on the bar and the title,
 *    never on the buttons (a drag region swallows the click that would have
 *    minimized the window);
 *  - **maximize/restore state** — the glyph has to follow the window, because
 *    with no native frame this is the only place that says which state you are
 *    in. `onResized` catches the cases that never touch this button: a Windows
 *    snap, Win+Up, a double-click on the drag region;
 *  - **resizing** — an undecorated window has no OS resize edges, which is what
 *    `ResizeEdges` below puts back.
 */
export default function TitleBar() {
  const win = getCurrentWindow();
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;

    const sync = () => {
      void win
        .isMaximized()
        .then((m) => { if (alive) setMaximized(m); })
        .catch(() => {});
    };
    sync();

    // Every path that changes the state, including the ones that never touch
    // our button: snapping, Win+Up, double-clicking the drag region.
    void win.onResized(sync).then((fn) => {
      if (alive) unlisten = fn;
      else fn();
    }).catch(() => {});

    return () => { alive = false; unlisten?.(); };
  }, [win]);

  // The frame is a window frame again, so it should stop pretending to be one
  // when maximized: no rounded corners against the screen edge, no drop shadow
  // under something with nothing beneath it.
  useEffect(() => {
    document.documentElement.setAttribute("data-maximized", maximized ? "true" : "false");
  }, [maximized]);

  return (
    <>
      <div className="titlebar" data-tauri-drag-region>
        <span className="titlebar-title" data-tauri-drag-region>Farder</span>
        <div className="titlebar-buttons">
          <button
            className="titlebar-btn"
            onClick={() => void win.minimize()}
            title="Minimize"
            aria-label="Minimize"
          >
            _
          </button>
          <button
            className="titlebar-btn"
            onClick={() => void win.toggleMaximize()}
            title={maximized ? "Restore" : "Maximize"}
            aria-label={maximized ? "Restore" : "Maximize"}
          >
            {maximized ? "❐" : "□"}
          </button>
          <button
            className="titlebar-btn close"
            onClick={() => void win.close()}
            title="Close"
            aria-label="Close"
          >
            ✕
          </button>
        </div>
      </div>
      {!maximized && <ResizeEdges />}
    </>
  );
}

/** The eight grab areas a native frame gives you for free. */
type Edge = {
  dir: "North" | "South" | "East" | "West" | "NorthEast" | "NorthWest" | "SouthEast" | "SouthWest";
  style: React.CSSProperties;
};

const THICK = 5;
const CORNER = 12;

const EDGES: Edge[] = [
  { dir: "North", style: { top: 0, left: CORNER, right: CORNER, height: THICK, cursor: "ns-resize" } },
  { dir: "South", style: { bottom: 0, left: CORNER, right: CORNER, height: THICK, cursor: "ns-resize" } },
  { dir: "West", style: { left: 0, top: CORNER, bottom: CORNER, width: THICK, cursor: "ew-resize" } },
  { dir: "East", style: { right: 0, top: CORNER, bottom: CORNER, width: THICK, cursor: "ew-resize" } },
  { dir: "NorthWest", style: { top: 0, left: 0, width: CORNER, height: CORNER, cursor: "nwse-resize" } },
  { dir: "NorthEast", style: { top: 0, right: 0, width: CORNER, height: CORNER, cursor: "nesw-resize" } },
  { dir: "SouthWest", style: { bottom: 0, left: 0, width: CORNER, height: CORNER, cursor: "nesw-resize" } },
  { dir: "SouthEast", style: { bottom: 0, right: 0, width: CORNER, height: CORNER, cursor: "nwse-resize" } },
];

/**
 * Invisible grab strips around the window edge.
 *
 * Deliberately styled inline: they carry no colour and no theme has an opinion
 * about them, so a class here would be a class with no CSS — which in this
 * codebase renders as a visible raw element in every theme that forgot it.
 *
 * Only mounted when the window is restored. A maximized window has no edges to
 * drag, and leaving them mounted puts an invisible 12px dead zone over the
 * corners of a maximized app — including where the close button lives.
 */
function ResizeEdges() {
  const win = getCurrentWindow();

  return (
    <>
      {EDGES.map((e) => (
        <div
          key={e.dir}
          onMouseDown={(ev) => {
            // Left button only: a right-click here should reach whatever is
            // underneath, not begin a resize the user cannot see.
            if (ev.button !== 0) return;
            ev.preventDefault();
            void win.startResizeDragging(e.dir);
          }}
          style={{ position: "fixed", zIndex: 10000, ...e.style }}
        />
      ))}
    </>
  );
}
