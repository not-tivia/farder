import { useDensity } from "../../hooks/useDensity";
import { DENSITIES, DENSITY_INFO, setDensity } from "../../lib/density";
import RadioOption from "./RadioOption";

/**
 * How much room each message takes, from roomy down to one tiny line. Applies
 * everywhere messages are shown and is remembered on this device.
 */
export default function DensitySettings() {
    const current = useDensity();
    return (
        <div className="settings-panel">
            <h2 className="settings-panel-title">Message density</h2>
            <div className="connect-section">
                <div className="connect-section-title">How much room each message takes</div>
                {DENSITIES.map((d) => (
                    <RadioOption
                        key={d}
                        selected={current === d}
                        label={DENSITY_INFO[d].label}
                        description={DENSITY_INFO[d].description}
                        onSelect={() => setDensity(d)}
                    />
                ))}
            </div>
        </div>
    );
}
