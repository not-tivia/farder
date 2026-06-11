import { useState, useRef, useEffect } from "react";
import * as api from "../lib/tauri-bridge";

interface Props {
    onRecorded: (filePath: string, duration: number) => void;
    onCancel: () => void;
}

export default function VoiceRecorder({ onRecorded, onCancel }: Props) {
    const [recording, setRecording] = useState(true);
    const [duration, setDuration] = useState(0);
    const [error, setError] = useState<string | null>(null);
    const [filePath, setFilePath] = useState<string | null>(null);
    const [previewing, setPreviewing] = useState(false);
    const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
    // True only while THIS component instance owns a live backend recording.
    // The unmount cleanup must stop ONLY a recording it owns: a blanket stop
    // raced React StrictMode's dev double-mount (mount A's cleanup landed
    // after mount B's start and killed B's recording, so Stop then reported
    // "no recording in progress").
    const ownsRecordingRef = useRef(false);

    useEffect(() => {
        let active = true;
        (async () => {
            try {
                // Start the backend recording. If a previous instance left one
                // wedged (StrictMode remount, crashed component), recover by
                // stopping it and starting fresh instead of failing.
                try {
                    await api.startRecording();
                } catch (e) {
                    if (String(e).includes("already recording")) {
                        await api.stopRecording().catch(() => {});
                        await api.startRecording();
                    } else {
                        throw e;
                    }
                }
                if (!active) {
                    // Unmounted while starting (StrictMode): release the backend
                    // recording so the surviving mount can start cleanly.
                    api.stopRecording().catch(() => {});
                    return;
                }
                ownsRecordingRef.current = true;
                timerRef.current = setInterval(() => {
                    setDuration(prev => prev + 1);
                }, 1000);
            } catch (e) {
                if (active) {
                    setError(String(e));
                    setRecording(false);
                }
            }
        })();
        return () => {
            active = false;
            if (timerRef.current) clearInterval(timerRef.current);
            if (ownsRecordingRef.current) {
                ownsRecordingRef.current = false;
                api.stopRecording().catch(() => {});
            }
        };
    }, []);

    async function handleStop() {
        if (timerRef.current) clearInterval(timerRef.current);
        ownsRecordingRef.current = false;
        try {
            const path = await api.stopRecording();
            setFilePath(path);
            setRecording(false);
        } catch (e) {
            setError(String(e));
            setRecording(false);
        }
    }

    async function handlePreview() {
        if (!filePath || previewing) return;
        setPreviewing(true);
        try {
            await api.playAudioFile(filePath);
        } catch (e) {
            console.error("[voice] preview playback failed:", e);
        } finally {
            setPreviewing(false);
        }
    }

    function handleSend() {
        if (filePath) {
            onRecorded(filePath, duration);
        }
    }

    function handleCancel() {
        if (timerRef.current) clearInterval(timerRef.current);
        if (ownsRecordingRef.current) {
            ownsRecordingRef.current = false;
            api.stopRecording().catch(() => {});
        }
        onCancel();
    }

    function formatDuration(secs: number): string {
        const m = Math.floor(secs / 60);
        const s = secs % 60;
        return `${m}:${s.toString().padStart(2, "0")}`;
    }

    if (error) {
        return (
            <div className="voice-recorder">
                <span style={{ color: "var(--danger, #cc0000)", flex: 1 }}>Recording failed: {error}</span>
                <button className="xp-button" onClick={onCancel}>Close</button>
            </div>
        );
    }

    return (
        <div className="voice-recorder">
            {recording ? (
                <>
                    <span className="voice-recording-dot" />
                    <span className="voice-timer">{formatDuration(duration)}</span>
                    <button className="xp-button" onClick={handleStop}>Stop</button>
                    <button className="xp-button" onClick={handleCancel}>Cancel</button>
                </>
            ) : (
                <>
                    <span className="voice-timer">{formatDuration(duration)}</span>
                    <button className="xp-button" onClick={handlePreview} disabled={previewing}>
                        {previewing ? "Playing..." : "Preview"}
                    </button>
                    <button className="xp-button" onClick={handleSend}>Send</button>
                    <button className="xp-button" onClick={handleCancel}>Discard</button>
                </>
            )}
        </div>
    );
}
