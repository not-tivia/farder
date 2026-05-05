import { useEffect, useState } from "react";
import * as api from "../lib/tauri-bridge";
import type { AttachmentInfo } from "../lib/types";

// Module-level cache shared across all InlineBookEmoji instances so a single
// attachment isn't downloaded multiple times even if it appears in many messages.
const inlineImageCache = new Map<number, string>();

interface Props {
  attachment: AttachmentInfo;
  serverId: string;
  altText: string;
}

export default function InlineBookEmoji({ attachment, serverId, altText }: Props) {
  const [imageUrl, setImageUrl] = useState<string | null>(
    inlineImageCache.get(attachment.file_id) ?? null,
  );

  useEffect(() => {
    if (imageUrl != null) return;
    let cancelled = false;
    api.downloadFile(serverId, attachment.file_id).then((r) => {
      if (!cancelled && r.data_url) {
        inlineImageCache.set(attachment.file_id, r.data_url);
        setImageUrl(r.data_url);
      }
    }).catch(() => {});
    return () => { cancelled = true; };
  }, [attachment.file_id, serverId, imageUrl]);

  if (imageUrl == null) {
    return <span style={{ fontSize: 10, opacity: 0.5 }}>…</span>;
  }

  return (
    <img
      src={imageUrl}
      alt={altText}
      title={altText}
      className="inline-book-emoji"
      style={{
        width: "1.4em",
        height: "1.4em",
        objectFit: "contain",
        verticalAlign: "middle",
        margin: "0 1px",
      }}
    />
  );
}
