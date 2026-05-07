import type { CSSProperties } from "react";

interface Props {
  untilMs?: number | null;
  reason?: string | null;
}

const badge: CSSProperties = {
  display: "inline-block",
  fontSize: 11,
  marginLeft: 4,
  cursor: "help",
  opacity: 0.85,
};

export default function TimedOutBadge({ untilMs, reason }: Props) {
  if (!untilMs || untilMs <= Date.now()) return null;
  const until = new Date(untilMs).toLocaleString();
  const tip = reason
    ? `Timed out until ${until} · ${reason}`
    : `Timed out until ${until}`;
  return (
    <span style={badge} title={tip} aria-label={tip}>
      ⏱
    </span>
  );
}
