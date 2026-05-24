// client/src/components/TranslationDownloadDialog.tsx

import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { displayName } from "../lib/translation/lang";
import type { DownloadProgress } from "../lib/translation/types";

interface Props {
  pair: { src: string; trg: string };
  onConfirm: () => void;
  onCancel: () => void;
  inProgress: boolean;  // true once user accepted; show progress instead of confirm
}

export function TranslationDownloadDialog({ pair, onConfirm, onCancel, inProgress }: Props) {
  const [progress, setProgress] = useState<DownloadProgress | null>(null);

  useEffect(() => {
    if (!inProgress) return;
    const unlistenP = listen<DownloadProgress>("translation:progress", (evt) => {
      const p = evt.payload;
      if (p.pair.src === pair.src && p.pair.trg === pair.trg) {
        setProgress(p);
      }
    });
    return () => { unlistenP.then((u) => u()); };
  }, [inProgress, pair.src, pair.trg]);

  return (
    <div className="modal-overlay" style={overlayStyle}>
      <div className="modal" style={modalStyle}>
        <h3>Download translation model?</h3>
        <p>
          Downloading the {displayName(pair.src)} ↔ {displayName(pair.trg)} model from
          Mozilla's servers (storage.googleapis.com).
        </p>
        <p style={{ fontSize: "0.9em", color: "var(--text-muted, #888)" }}>
          Mozilla will see your IP address and the language pair you're requesting.
        </p>
        <p style={{ fontSize: "0.9em", color: "var(--text-muted, #888)" }}>
          Once downloaded, all translation runs entirely on your device — no chat
          content is ever sent anywhere.
        </p>
        {inProgress && progress && (
          <div style={{ margin: "12px 0" }}>
            <div>{progress.stage}: {progress.message ?? ""}</div>
            <progress
              value={progress.bytes_done}
              max={progress.bytes_total || 1}
              style={{ width: "100%" }}
            />
          </div>
        )}
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", marginTop: 16 }}>
          <button onClick={onCancel} disabled={inProgress}>Cancel</button>
          <button onClick={onConfirm} disabled={inProgress}>Download</button>
        </div>
      </div>
    </div>
  );
}

const overlayStyle: React.CSSProperties = {
  position: "fixed", inset: 0,
  background: "rgba(0,0,0,0.5)",
  display: "flex", alignItems: "center", justifyContent: "center",
  zIndex: 1000,
};

const modalStyle: React.CSSProperties = {
  background: "var(--bg-elevated, #fff)",
  color: "var(--text, #000)",
  padding: 20,
  borderRadius: 8,
  maxWidth: 480,
  width: "90%",
};
