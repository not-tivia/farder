import { useEffect, useRef, useState, type CSSProperties } from "react";

interface Props {
  /** Participant pubkey hex (the same string the Rust controller keys by). */
  pubkeyHex: string;
  displayName: string;
  position: { x: number; y: number };
  /** Current playback gain (1.0 = 100%). */
  currentVolume?: number;
  /** Persist + apply a new gain for this peer. */
  onSetVolume?: (volume: number) => void;
  onClose: () => void;
}

const menuStyle: CSSProperties = {
  position: "fixed",
  background: "var(--xp-panel-bg, #fff)",
  color: "var(--xp-text-normal, #000)",
  border: "1px solid var(--xp-border, #888)",
  boxShadow: "2px 2px 8px rgba(0,0,0,0.3)",
  padding: 4,
  minWidth: 180,
  zIndex: 2500,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
  fontSize: "var(--xp-font-size, 11px)",
};

const headerStyle: CSSProperties = {
  padding: "4px 10px",
  fontWeight: "bold",
  whiteSpace: "nowrap",
  overflow: "hidden",
  textOverflow: "ellipsis",
};

export default function VoiceParticipantContextMenu({
  pubkeyHex,
  displayName,
  position,
  currentVolume,
  onSetVolume,
  onClose,
}: Props) {
  const ref = useRef<HTMLDivElement | null>(null);
  // Local slider state for smooth dragging; seeded from the saved volume.
  const [pct, setPct] = useState<number>(Math.round((currentVolume ?? 1) * 100));

  useEffect(() => {
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

  return (
    <div ref={ref} style={{ ...menuStyle, top: position.y, left: position.x }}>
      <div style={headerStyle} title={pubkeyHex}>
        {displayName}
      </div>
      {onSetVolume && (
        <div className="voice-volume-slider">
          <label>Volume: {pct}%</label>
          <input
            type="range"
            min={0}
            max={200}
            step={5}
            value={pct}
            onChange={(e) => {
              const next = Number(e.target.value);
              setPct(next);
              onSetVolume(next / 100);
            }}
          />
        </div>
      )}
    </div>
  );
}
