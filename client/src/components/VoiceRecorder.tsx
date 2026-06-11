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
    // The session id of the recording THIS instance owns (from startRecording).
    // All stops pass it, so a late/stray stop (React StrictMode's dev
    // double-mount, async cleanup landing after a newer mount started) is
    // rejected by the backend instead of killing a recording it doesn't own.
    const sessionRef = useRef<number | null>(null);

    useEffect(() => {
        let active = true;
        (async () => {
            try {
                // Start the backend recording. If a previous instance left one
                // wedged (crashed component, orphaned mount), recover by
                // stopping it (token-less = stop whatever is live) and retrying.
                let session: number;
                try {
                    session = await api.startRecording();
                } catch (e) {
                    if (String(e).includes("already recording")) {
                        await api.stopRecording().catch(() => {});
                        session = await api.startRecording();
                    } else {
                        throw e;
                    }
                }
                if (!active) {
                    // Unmounted while starting (StrictMode): release OUR session.
                    // The token makes this safe — if a newer mount already started
                    // its own recording, this stop is rejected as stale.
                    api.stopRecording(session).catch(() => {});
                    return;
                }
                sessionRef.current = session;
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
            if (sessionRef.current !== null) {
                const session = sessionRef.current;
                sessionRef.current = null;
                api.stopRecording(session).catch(() => {});
            }
        };
    }, []);

    async function handleStop() {
        if (timerRef.current) clearInterval(timerRef.current);
        const session = sessionRef.current ?? undefined;
        sessionRef.current = null;
        try {
            const path = await api.stopRecording(session);
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
        if (sessionRef.current !== null) {
            const session = sessionRef.current;
            sessionRef.current = null;
            api.stopRecording(session).catch(() => {});
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
