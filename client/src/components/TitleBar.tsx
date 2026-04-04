import { getCurrentWindow } from "@tauri-apps/api/window";

export default function TitleBar() {
  const win = getCurrentWindow();

  return (
    <div className="titlebar">
      <span className="titlebar-title">Farder</span>
      <div className="titlebar-buttons">
        <button className="titlebar-btn" onClick={() => win.minimize()} title="Minimize">
          _
        </button>
        <button className="titlebar-btn" onClick={() => win.toggleMaximize()} title="Maximize">
          □
        </button>
        <button className="titlebar-btn close" onClick={() => win.close()} title="Close">
          ✕
        </button>
      </div>
    </div>
  );
}
