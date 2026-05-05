import { useEffect, useRef, useState, type CSSProperties } from "react";
import * as api from "../lib/tauri-bridge";
import * as gifApi from "../lib/gifSearch";
import * as bookApi from "../lib/book/client";
import type { TenorGif, GifSearchSettings } from "../lib/gifSearch";

interface Props {
  serverId: string;
  channelId: number;
  onClose: () => void;
}

const popover: CSSProperties = {
  position: "absolute",
  bottom: "calc(100% + 4px)",
  left: 0,
  background: "var(--xp-panel-bg, #fff)",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  boxShadow: "2px 2px 8px rgba(0,0,0,0.3)",
  padding: 8,
  width: 360,
  maxHeight: 420,
  zIndex: 1100,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
  fontSize: "var(--xp-font-size, 11px)",
  display: "flex",
  flexDirection: "column",
  gap: 6,
};

function GifTile({
  gif,
  onSend,
  onSave,
}: {
  gif: TenorGif;
  onSend: (g: TenorGif) => void;
  onSave: (g: TenorGif) => void;
}) {
  const [hover, setHover] = useState(false);
  return (
    <div
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        position: "relative",
        width: "calc(50% - 4px)",
        cursor: "pointer",
        aspectRatio: gif.width && gif.height ? `${gif.width} / ${gif.height}` : "1 / 1",
      }}
      onClick={() => onSend(gif)}
      title={gif.title}
    >
      <img
        src={gif.preview_url}
        alt={gif.title}
        style={{ width: "100%", height: "100%", objectFit: "cover", border: "1px solid var(--xp-border, #888)" }}
      />
      {hover && (
        <button
          onClick={(e) => {
            e.stopPropagation();
            onSave(gif);
          }}
          title="Save to book"
          style={{
            position: "absolute",
            top: 4,
            right: 4,
            background: "rgba(0,0,0,0.7)",
            color: "#fff",
            border: "none",
            borderRadius: 4,
            padding: "2px 6px",
            cursor: "pointer",
            fontSize: 14,
          }}
        >
          📚
        </button>
      )}
    </div>
  );
}

export default function GifPicker({ serverId, channelId, onClose }: Props) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [search, setSearch] = useState("");
  const [debouncedSearch, setDebouncedSearch] = useState("");
  const [results, setResults] = useState<TenorGif[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [settings, setSettings] = useState<GifSearchSettings | null>(null);
  const reqSeqRef = useRef(0);

  useEffect(() => {
    gifApi.getGifSearchSettings().then(setSettings).catch(() => {});
    function handleMouse(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("mousedown", handleMouse);
    document.addEventListener("keydown", handleKey);
    return () => {
      document.removeEventListener("mousedown", handleMouse);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onClose]);

  useEffect(() => {
    const t = setTimeout(() => setDebouncedSearch(search), 300);
    return () => clearTimeout(t);
  }, [search]);

  useEffect(() => {
    const seq = ++reqSeqRef.current;
    setLoading(true);
    setError(null);
    const fetcher = debouncedSearch.trim()
      ? gifApi.tenorSearch(debouncedSearch.trim())
      : gifApi.tenorTrending();
    fetcher
      .then((r) => {
        if (seq !== reqSeqRef.current) return;
        setResults(r.gifs);
        setNext(r.next);
      })
      .catch((e) => {
        if (seq !== reqSeqRef.current) return;
        setError(String(e));
        setResults([]);
        setNext(null);
      })
      .finally(() => {
        if (seq === reqSeqRef.current) setLoading(false);
      });
  }, [debouncedSearch]);

  async function loadMore() {
    if (!next || loading) return;
    const seq = ++reqSeqRef.current;
    setLoading(true);
    try {
      const more = debouncedSearch.trim()
        ? await gifApi.tenorSearch(debouncedSearch.trim(), next)
        : await gifApi.tenorTrending(next);
      if (seq !== reqSeqRef.current) return;
      setResults((prev) => [...prev, ...more.gifs]);
      setNext(more.next);
    } catch (e) {
      if (seq === reqSeqRef.current) setError(String(e));
    } finally {
      if (seq === reqSeqRef.current) setLoading(false);
    }
  }

  function handleScroll(e: React.UIEvent<HTMLDivElement>) {
    const el = e.currentTarget;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 50) {
      void loadMore();
    }
  }

  async function send(gif: TenorGif) {
    try {
      const fileId = await api.fetchUrl(serverId, gif.full_url, channelId);
      await api.sendMessage(serverId, channelId, "", undefined, [fileId]);
      onClose();
    } catch (e) {
      setError(String(e));
    }
  }

  async function save(gif: TenorGif) {
    try {
      const fileId = await api.fetchUrl(serverId, gif.full_url, channelId);
      const safeName = gif.title.replace(/[^a-zA-Z0-9]+/g, "-").toLowerCase().slice(0, 32) || "tenor-gif";
      await bookApi.bookSaveFromUrl(serverId, fileId, safeName);
    } catch (e) {
      setError(String(e));
    }
  }

  const showNsfwWarning = settings && settings.content_filter !== "high";

  return (
    <div ref={ref} style={popover}>
      <input
        autoFocus
        placeholder="Search Tenor…"
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        style={{ font: "inherit", padding: "2px 6px" }}
      />
      {showNsfwWarning && (
        <div style={{ fontSize: 10, color: "#a60", background: "#fff8e1", padding: "2px 6px", border: "1px solid #e0c060" }}>
          Content filter is set to "{settings?.content_filter}". Adult content may appear.
        </div>
      )}
      {error && <div style={{ color: "#a00", fontSize: 10 }}>{error}</div>}
      <div
        onScroll={handleScroll}
        style={{ overflowY: "auto", display: "flex", flexWrap: "wrap", gap: 4 }}
      >
        {loading && results.length === 0 && (
          <div style={{ width: "100%", textAlign: "center", padding: 16, color: "var(--xp-text-muted, #666)" }}>
            Loading…
          </div>
        )}
        {!loading && !error && results.length === 0 && (
          <div style={{ width: "100%", textAlign: "center", padding: 16, color: "var(--xp-text-muted, #666)" }}>
            {debouncedSearch.trim() ? "No results." : "Try a search to find GIFs."}
          </div>
        )}
        {results.map((g) => (
          <GifTile key={g.id} gif={g} onSend={send} onSave={save} />
        ))}
        {loading && results.length > 0 && (
          <div style={{ width: "100%", textAlign: "center", padding: 8, color: "var(--xp-text-muted, #666)" }}>
            Loading more…
          </div>
        )}
      </div>
    </div>
  );
}
