import { useEffect, useState, type CSSProperties } from "react";

interface Props {
  untilMs: number;
  reason: string | null;
}

const banner: CSSProperties = {
  background: "#fff8e1",
  color: "#a60",
  border: "1px solid #e0c060",
  padding: "4px 8px",
  fontSize: 11,
  fontFamily: "var(--xp-font, Tahoma, sans-serif)",
};

function formatRemaining(ms: number): string {
  if (ms <= 0) return "0s";
  const totalSec = Math.ceil(ms / 1000);
  const days = Math.floor(totalSec / 86400);
  const hours = Math.floor((totalSec % 86400) / 3600);
  const mins = Math.floor((totalSec % 3600) / 60);
  const secs = totalSec % 60;
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${mins}m`;
  if (mins > 0) return `${mins}m ${secs}s`;
  return `${secs}s`;
}

export default function TimeoutBanner({ untilMs, reason }: Props) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);

  const remaining = untilMs - now;
  if (remaining <= 0) return null;

  return (
    <div style={banner}>
      <strong>You're timed out for {formatRemaining(remaining)}.</strong>
      {reason && <> Reason: {reason}</>}
    </div>
  );
}
