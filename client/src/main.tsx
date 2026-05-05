import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { getActiveTheme } from "./lib/tauri-bridge";
import { bookMigrateLegacyFavorites } from "./lib/book/client";

async function bootstrap() {
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
