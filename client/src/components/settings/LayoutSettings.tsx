import { useLayout } from "../../hooks/useLayout";
import { LAYOUTS, LAYOUT_INFO } from "../../lib/layout";
import { changeLayout } from "../../lib/layoutWindow";
import RadioOption from "./RadioOption";

/**
 * Full window or the small single-window layout. Changing it also resizes the
 * window; it is remembered on this device.
 */
export default function LayoutSettings() {
    const current = useLayout();
    return (
        <div className="settings-panel">
            <h2 className="settings-panel-title">Window layout</h2>
            <div className="connect-section">
                <div className="connect-section-title">How much of Farder is on screen</div>
                {LAYOUTS.map((l) => (
                    <RadioOption
                        key={l}
                        selected={current === l}
                        label={LAYOUT_INFO[l].label}
                        description={LAYOUT_INFO[l].description}
                        onSelect={() => changeLayout(l)}
                    />
                ))}
            </div>
        </div>
    );
}
