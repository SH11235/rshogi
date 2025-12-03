import type { EngineEvent, SearchHandle } from "@shogi/engine-client";
import { createTauriEngineClient } from "@shogi/engine-tauri";
import { useEffect, useMemo, useState } from "react";
import "./App.css";

const MAX_LOGS = 6;

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
    const engine = useMemo(
        () => createTauriEngineClient({ stopMode: "terminate", useMockOnError: false }),
        [],
    );
    const [status, setStatus] = useState<"idle" | "init" | "searching" | "error">("idle");
    const [bestmove, setBestmove] = useState<string | null>(null);
    const [logs, setLogs] = useState<string[]>([]);

    useEffect(() => {
        let handle: SearchHandle | null = null;
        const unsubscribe = engine.subscribe((event) => {
            setLogs((prev) => {
                const next = [...prev, formatEvent(event)];
                return next.length > MAX_LOGS ? next.slice(-MAX_LOGS) : next;
            });
            if (event.type === "bestmove") {
                setBestmove(event.move);
            }
        });

        (async () => {
            try {
                setStatus("init");
                await engine.init();
                await engine.loadPosition("startpos");
                setStatus("searching");
                handle = await engine.search({ limits: { maxDepth: 1 } });
            } catch (error) {
                setStatus("error");
                setLogs((prev) => [...prev.slice(-(MAX_LOGS - 1)), `error ${String(error)}`]);
            }
        })();

        return () => {
            if (handle) {
                handle.cancel().catch((err) => console.warn("Failed to cancel search:", err));
            }
            unsubscribe();
            engine.dispose().catch(() => undefined);
        };
    }, [engine]);

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
