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
    const runningRef = useRef<boolean>(false);

    useEffect(() => {
        const unsubscribe = engine.subscribe((event) => {
            // Debug log to see raw events in DevTools
            console.info("[ui] engine event", event);
            setLogs((prev) => {
                const line = formatEvent(event);
                const last = prev[prev.length - 1];
                if (last === line) return prev; // avoid duplicate adjacent lines
                const next = [...prev, line];
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

        return () => {
            const toCancel = handleRef.current;
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

    const startSearch = async () => {
        if (runningRef.current) return;
        runningRef.current = true;
        try {
            setStatus("init");
            await engine.init();
            await engine.loadPosition("startpos");
            setStatus("searching");
            const handle = await engine.search({ limits: { maxDepth: 1 } });
            handleRef.current = handle;
        } catch (error) {
            setStatus("error");
            setLogs((prev) => [...prev.slice(-(MAX_LOGS - 1)), `error ${String(error)}`]);
        } finally {
            runningRef.current = false;
        }
    };

    return (
        <main className="container">
            <h1>Engine wiring (Desktop / Tauri)</h1>
            <p>
                status: {status} {bestmove ? `| bestmove: ${bestmove}` : ""}
            </p>
            <p>
                <button type="button" onClick={startSearch} disabled={status === "searching"}>
                    Run debug search (startpos depth=1)
                </button>
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
