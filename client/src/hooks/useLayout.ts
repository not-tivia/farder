import { useEffect, useState } from "react";
import { LAYOUT_CHANGED, applyLayout, effectiveLayout, getLayout, type Layout } from "../lib/layout";

/** The layout being drawn: the saved choice, kept current when it changes anywhere in
 *  the app, except that a narrow screen always gets the small layout (a phone, or a
 *  very thin window), because the full layout pushes the chat off the screen. */
export function useLayout(): Layout {
  const [l, setL] = useState<Layout>(() => effectiveLayout(getLayout(), window.innerWidth));
  useEffect(() => {
    const sync = () => {
      const next = effectiveLayout(getLayout(), window.innerWidth);
      applyLayout(next);   // the stylesheet is keyed on <html data-layout>
      setL(next);
    };
    sync();
    window.addEventListener(LAYOUT_CHANGED, sync);
    window.addEventListener("resize", sync);
    return () => { window.removeEventListener(LAYOUT_CHANGED, sync); window.removeEventListener("resize", sync); };
  }, []);
  return l;
}
