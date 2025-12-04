import type { EngineEvent, SearchHandle } from "@shogi/engine-client";
import { createTauriEngineClient } from "@shogi/engine-tauri";
import { useEffect, useRef, useState } from "react";
import "./App.css";

const MAX_LOGS = 6;
const engine = createTauriEngineClient({
    stopMode: "terminate",
    useMockOnError: false,
    debug: true,
});

function formatEvent(event: EngineEvent): string {
    if (event.type === "bestmove") {
        return `bestmove ${event.move}${event.ponder ? ` ponder ${event.ponder}` : ""}`;
    }
    if (event.type === "info") {
        const score =
            event.scoreMate !== undefined
                ? `mate ${event.scoreMate}`
                : event.scoreCp !== undefined
                  ? `cp ${event.scoreCp}`
                  : "";
        return `info depth ${event.depth ?? "-"} nodes ${event.nodes ?? "-"} ${score}`;
    }
    return `error ${event.message}`;
}

function App() {
    const [status, setStatus] = useState<"idle" | "init" | "searching" | "error">("idle");
    const [bestmove, setBestmove] = useState<string | null>(null);
    const [logs, setLogs] = useState<string[]>([]);
    const handleRef = useRef<SearchHandle | null>(null);
    const sessionRef = useRef<number>(0);

    useEffect(() => {
        const sessionId = sessionRef.current + 1;
        sessionRef.current = sessionId;
        let cancelled = false;
        let handle: SearchHandle | null = null;
        const unsubscribe = engine.subscribe((event) => {
            if (sessionId !== sessionRef.current) return;
            // Debug log to see raw events in DevTools
            console.info("[ui] engine event", event);
            setLogs((prev) => {
                const next = [...prev, formatEvent(event)];
                return next.length > MAX_LOGS ? next.slice(-MAX_LOGS) : next;
            });
            if (event.type === "bestmove") {
                setBestmove(event.move);
                setStatus("idle");
            }
            if (event.type === "error") {
                setStatus("error");
            }
        });

        (async () => {
            try {
                if (sessionId !== sessionRef.current) return;
                setStatus("init");
                await engine.init();
                if (sessionId !== sessionRef.current) return;
                await engine.loadPosition("startpos");
                if (sessionId !== sessionRef.current) return;
                setStatus("searching");
                const h = await engine.search({ limits: { maxDepth: 1 } });
                if (cancelled) {
                    await h.cancel().catch(() => undefined);
                    return;
                }
                handle = h;
                handleRef.current = h;
            } catch (error) {
                if (sessionId !== sessionRef.current) return;
                setStatus("error");
                setLogs((prev) => [...prev.slice(-(MAX_LOGS - 1)), `error ${String(error)}`]);
            }
        })();

        return () => {
            cancelled = true;
            const toCancel = handle ?? handleRef.current;
            if (toCancel) {
                toCancel
                    .cancel()
                    .catch((err) => console.warn("Failed to cancel search:", err))
                    .finally(() => {
                        if (handleRef.current === toCancel) {
                            handleRef.current = null;
                        }
                    });
            }
            unsubscribe();
        };
    }, []);

    return (
        <main className="container">
            <h1>Engine wiring (Desktop / Tauri)</h1>
            <p>
                status: {status} {bestmove ? `| bestmove: ${bestmove}` : ""}
            </p>
            <section className="logs">
                <h2>Engine events (native backend)</h2>
                <ul>
                    {logs.map((line, idx) => (
                        <li key={`${idx}-${line}`}>{line}</li>
                    ))}
                </ul>
            </section>
        </main>
    );
}

export default App;
