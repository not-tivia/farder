import { usePip } from "../context/PipContext";
import PipPane from "./PipPane";

// Renders every open PiP pane. Mounted once in AppShell (overlay level) so panes
// float above all views and persist across channel/server navigation.
export default function PipLayer() {
  const { panes, closePip, focusPip, updatePip } = usePip();
  if (panes.length === 0) return null;
  return (
    <>
      {panes.map((p) => (
        <PipPane key={p.id} pane={p} onClose={closePip} onFocus={focusPip} onUpdate={updatePip} />
      ))}
    </>
  );
}
