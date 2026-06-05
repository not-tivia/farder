import { useEffect, useState } from "react";
import type { ToastEvent } from "../lib/toast";

const AUTO_DISMISS_MS = 4500;
const MAX_VISIBLE = 4;

/**
 * Renders the stack of active toasts (bottom-right), auto-dismissing each after
 * a few seconds. Mount once at the app root. Subscribes to the `farder:toast`
 * window event emitted by `lib/toast.ts` — independent of any React context.
 */
export default function ToastContainer() {
  const [toasts, setToasts] = useState<ToastEvent[]>([]);

  useEffect(() => {
    const handler = (e: Event) => {
      const detail = (e as CustomEvent<ToastEvent>).detail;
      if (!detail) return;
      setToasts((prev) => [...prev, detail].slice(-MAX_VISIBLE));
      window.setTimeout(() => {
        setToasts((prev) => prev.filter((t) => t.id !== detail.id));
      }, AUTO_DISMISS_MS);
    };
    window.addEventListener("farder:toast", handler);
    return () => window.removeEventListener("farder:toast", handler);
  }, []);

  if (toasts.length === 0) return null;

  const dismiss = (id: number) => setToasts((prev) => prev.filter((t) => t.id !== id));

  return (
    <div className="toast-container">
      {toasts.map((t) => (
        <div key={t.id} className={`toast toast-${t.variant}`} role="status">
          <span className="toast-message">{t.message}</span>
          <button className="toast-close" title="Dismiss" onClick={() => dismiss(t.id)}>
            &#10005;
          </button>
        </div>
      ))}
    </div>
  );
}
