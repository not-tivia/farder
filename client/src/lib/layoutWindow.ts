import * as api from "./tauri-bridge";
import { LAYOUT_CSS, applyLayout, getLayout, setLayout, type Layout } from "./layout";

/** Switch layout: remember it, redraw, and resize the window to suit. */
export function changeLayout(l: Layout): void {
  setLayout(l);
  api.setWindowLayout(l === "small").catch((e) => console.warn("[layout] could not resize the window:", e));
}

/** Called once at startup: injects the stylesheet and applies the saved choice. */
export function initLayout(): void {
  if (!document.getElementById("layout-css")) {
    const style = document.createElement("style");
    style.id = "layout-css";
    style.textContent = LAYOUT_CSS;
    document.head.appendChild(style);
  }
  const l = getLayout();
  applyLayout(l);
  // The window always starts at its default size, so a saved small layout has to
  // shrink it again (and lower the minimum size) on every launch.
  if (l === "small") api.setWindowLayout(true).catch(() => {});
}
