import { useEffect, useState } from "react";
import { DENSITY_CHANGED, getDensity, type Density } from "../lib/density";

/** The message density, kept current when it changes anywhere in the app. */
export function useDensity(): Density {
  const [d, setD] = useState<Density>(getDensity);
  useEffect(() => {
    const onChange = () => setD(getDensity());
    window.addEventListener(DENSITY_CHANGED, onChange);
    return () => window.removeEventListener(DENSITY_CHANGED, onChange);
  }, []);
  return d;
}
