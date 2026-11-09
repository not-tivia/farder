import { useEffect, useState } from "react";
import { LAYOUT_CHANGED, getLayout, type Layout } from "../lib/layout";

/** The window layout, kept current when it changes anywhere in the app. */
export function useLayout(): Layout {
  const [l, setL] = useState<Layout>(getLayout);
  useEffect(() => {
    const onChange = () => setL(getLayout());
    window.addEventListener(LAYOUT_CHANGED, onChange);
    return () => window.removeEventListener(LAYOUT_CHANGED, onChange);
  }, []);
  return l;
}
