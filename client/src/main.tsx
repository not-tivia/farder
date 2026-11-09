import React from "react";
import { initDensity } from "./lib/density";
import { initLayout } from "./lib/layoutWindow";
import ReactDOM from "react-dom/client";
import App from "./App";
import ScreenSharePopout from "./components/ScreenSharePopout";
import NotifyPopup from "./components/NotifyPopup";
import EdgeStrip from "./components/EdgeStrip";
import BuddyList from "./components/BuddyList";
import ChatWindow from "./components/ChatWindow";
import { getActiveTheme } from "./lib/tauri-bridge";
import { bookMigrateLegacyFavorites } from "./lib/book/client";
import { initAudioLimits } from "./lib/audioLimiter";

/** The buddy list and chat windows: say so on screen if anything goes wrong, instead of staying blank. */
function showSmallWindowDiagnostics() {
  const q = new URLSearchParams(window.location.search);
  if (q.get("buddy") !== "1" && q.get("chat") !== "1") return;
  const root = document.getElementById("root");
  if (root) { root.style.cssText = "padding:10px;font:13px sans-serif;color:#222;background:#fff;height:100vh;box-sizing:border-box;overflow:auto"; root.textContent = "Starting..."; }
  // Appended to <body>, not #root: React clears its own container when it gives up.
  const report = (what: unknown) => {
    const msg = what instanceof Error ? `${what.message}\n${what.stack ?? ""}` : String(what);
    let box = document.getElementById("small-window-error");
    if (!box) {
      box = document.createElement("pre");
      box.id = "small-window-error";
      box.style.cssText = "position:fixed;left:0;right:0;bottom:0;max-height:70vh;overflow:auto;margin:0;padding:8px;white-space:pre-wrap;background:#fff;color:#a00;font:11px monospace;z-index:99999";
      document.body.appendChild(box);
    }
    box.textContent += "Something went wrong in this window:\n" + msg.slice(0, 1500) + "\n\n";
  };
  window.addEventListener("error", (e) => report(e.error ?? e.message));
  window.addEventListener("unhandledrejection", (e) => report(e.reason));
}

async function bootstrap() {
  showSmallWindowDiagnostics();
  // Detached screen-share preview window: render JUST the popout view (no
  // servers/voice/full app) so it only decodes the app-wide frame events.
  if (new URLSearchParams(window.location.search).get("popout") === "screenshare") {
    document.documentElement.style.background = "#000";
    ReactDOM.createRoot(document.getElementById("root")!).render(<ScreenSharePopout />);
    return;
  }
  // Inject the active theme's CSS before React mounts so there's no
  // flash of default styling. If the IPC fails (shouldn't happen in
  // production), we still render — an unstyled app is better than a
  // blank window.
  try {
    const { id, css } = await getActiveTheme();
    const style = document.createElement("style");
    style.id = "active-theme";
    style.textContent = css;
    document.head.appendChild(style);
    document.documentElement.dataset.theme = id;
  } catch (e) {
    console.error("[bootstrap] failed to load theme:", e);
  }

  // Search-highlight flash animation. Lives outside theme CSS because it's
  // theme-independent — themes may override `.search-highlight` to change the
  // color, but the keyframes themselves are universal.
  const searchHighlight = document.createElement("style");
  searchHighlight.id = "search-highlight-keyframes";
  searchHighlight.textContent = `
    @keyframes farderSearchFlash {
      0%   { background-color: rgba(255, 165, 0, 0.45); }
      100% { background-color: transparent; }
    }
    .message.search-highlight {
      animation: farderSearchFlash 1.2s ease-out;
    }
  `;
  document.head.appendChild(searchHighlight);

  // The skinned notification window: JUST the pop-up view, on a transparent
  // page so only the cards are visible. It takes the active theme injected above.
  if (new URLSearchParams(window.location.search).get("notify") === "1") {
    document.documentElement.style.setProperty("background", "transparent", "important");
    document.body.style.setProperty("background", "transparent", "important");
    ReactDOM.createRoot(document.getElementById("root")!).render(<NotifyPopup />);
    return;
  }

  // The edge strip window: one line, docked to a screen edge (edge_strip.rs).
  if (new URLSearchParams(window.location.search).get("strip") === "1") {
    document.documentElement.style.setProperty("overflow", "hidden", "important");
    document.body.style.setProperty("margin", "0", "important");
    document.body.style.setProperty("overflow", "hidden", "important");
    ReactDOM.createRoot(document.getElementById("root")!).render(<EdgeStrip />);
    return;
  }

  // The buddy list and its chat windows (buddy.rs): small windows that only draw
  // what the main window pushes to them.
  const kind = new URLSearchParams(window.location.search);
  if (kind.get("buddy") === "1" || kind.get("chat") === "1") {
    document.documentElement.style.setProperty("overflow", "hidden", "important");
    document.body.style.setProperty("margin", "0", "important");
    document.body.style.setProperty("overflow", "hidden", "important");
    // Drop the plain "Starting..." text and its box styling (see showSmallWindowDiagnostics).
    const rootEl = document.getElementById("root")!;
    rootEl.style.cssText = "";
    rootEl.textContent = "";
    ReactDOM.createRoot(rootEl).render(kind.get("buddy") === "1" ? <BuddyList /> : <ChatWindow />);
    return;
  }

  // Loudness limiter for videos and voice messages in chat (a no-op at defaults).
  void initAudioLimits();
  // Message density (roomy ... tiny): stylesheet + the saved choice.
  initDensity();
  // Window layout (full / small window): stylesheet, saved choice, window size.
  initLayout();

  // One-time migration of legacy ~/.farder/favorites.json into the new book.
  // No-op after the first successful run (server renames favorites.json → .bak).
  try {
    const imported = await bookMigrateLegacyFavorites();
    if (imported > 0) {
      console.log(`[bootstrap] migrated ${imported} legacy favorites into the book`);
    }
  } catch (e) {
    console.warn("[bootstrap] favorites migration failed (non-fatal):", e);
  }

  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

bootstrap();
