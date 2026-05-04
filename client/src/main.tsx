import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { getActiveTheme } from "./lib/tauri-bridge";

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

  ReactDOM.createRoot(document.getElementById("root")!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

bootstrap();
