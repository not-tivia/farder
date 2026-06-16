import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import ScreenSharePopout from "./components/ScreenSharePopout";
import { getActiveTheme } from "./lib/tauri-bridge";
import { bookMigrateLegacyFavorites } from "./lib/book/client";

async function bootstrap() {
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
