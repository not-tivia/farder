import { useState, useRef, useEffect } from "react";

interface Props {
    onRecorded: (blob: Blob, duration: number) => void;
    onCancel: () => void;
}

export default function VoiceRecorder({ onRecorded, onCancel }: Props) {
    const [recording, setRecording] = useState(true);
    const [duration, setDuration] = useState(0);
    const [audioUrl, setAudioUrl] = useState<string | null>(null);
    const [audioBlob, setAudioBlob] = useState<Blob | null>(null);
    const mediaRecorderRef = useRef<MediaRecorder | null>(null);
    const chunksRef = useRef<Blob[]>([]);
    const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
    const startTimeRef = useRef(Date.now());

    useEffect(() => {
        startRecording();
        return () => {
            if (timerRef.current) clearInterval(timerRef.current);
            if (mediaRecorderRef.current && mediaRecorderRef.current.state === "recording") {
                mediaRecorderRef.current.stop();
            }
        };
    }, []);

    async function startRecording() {
        try {
            const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
            const mediaRecorder = new MediaRecorder(stream, {
                mimeType: MediaRecorder.isTypeSupported("audio/webm;codecs=opus")
                    ? "audio/webm;codecs=opus"
                    : "audio/webm"
            });
            mediaRecorderRef.current = mediaRecorder;
            chunksRef.current = [];
            startTimeRef.current = Date.now();

            mediaRecorder.ondataavailable = (e) => {
                if (e.data.size > 0) chunksRef.current.push(e.data);
            };

            mediaRecorder.onstop = () => {
                stream.getTracks().forEach(t => t.stop());
                const blob = new Blob(chunksRef.current, { type: mediaRecorder.mimeType });
                setAudioBlob(blob);
                setAudioUrl(URL.createObjectURL(blob));
                setRecording(false);
            };

            mediaRecorder.start(100); // collect data every 100ms

            timerRef.current = setInterval(() => {
                setDuration(Math.floor((Date.now() - startTimeRef.current) / 1000));
            }, 1000);
        } catch (e) {
            console.error("mic access failed:", e);
            onCancel();
        }
    }

    function handleStop() {
        if (timerRef.current) clearInterval(timerRef.current);
        if (mediaRecorderRef.current && mediaRecorderRef.current.state === "recording") {
            mediaRecorderRef.current.stop();
        }
    }

    function handleSend() {
        if (audioBlob) {
            onRecorded(audioBlob, duration);
        }
    }

    function handleCancel() {
        if (timerRef.current) clearInterval(timerRef.current);
        if (mediaRecorderRef.current && mediaRecorderRef.current.state === "recording") {
            mediaRecorderRef.current.stop();
        }
        if (audioUrl) URL.revokeObjectURL(audioUrl);
        onCancel();
    }

    function formatDuration(secs: number): string {
        const m = Math.floor(secs / 60);
        const s = secs % 60;
        return `${m}:${s.toString().padStart(2, "0")}`;
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
                    <audio src={audioUrl!} controls style={{ height: 28, flex: 1 }} />
                    <span className="voice-timer">{formatDuration(duration)}</span>
                    <button className="xp-button" onClick={handleSend}>Send</button>
                    <button className="xp-button" onClick={handleCancel}>Cancel</button>
                </>
            )}
        </div>
    );
}
