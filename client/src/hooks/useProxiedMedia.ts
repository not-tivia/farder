import { useEffect, useState } from "react";
import { getProxiedMedia } from "../lib/tauri-bridge";

/** Fetch media bytes via the relay and expose a blob URL; revokes on cleanup. */
export function useProxiedMedia(url: string | null, enabled: boolean): string | null {
  const [blobUrl, setBlobUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!url || !enabled) { setBlobUrl(null); return; }
    let alive = true;
    let created: string | null = null;
    getProxiedMedia(url)
      .then(({ content_type, data_base64 }) => {
        if (!alive) return;
        const bytes = Uint8Array.from(atob(data_base64), (c) => c.charCodeAt(0));
        const blob = new Blob([bytes], { type: content_type });
        created = URL.createObjectURL(blob);
        setBlobUrl(created);
      })
      .catch(() => { if (alive) setBlobUrl(null); });
    return () => { alive = false; if (created) URL.revokeObjectURL(created); };
  }, [url, enabled]);
  return blobUrl;
}
