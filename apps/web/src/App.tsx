import type { EngineEvent, SearchHandle } from "@shogi/engine-client";
import { createWasmEngineClient } from "@shogi/engine-wasm";
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
        () =>
            createWasmEngineClient({
                stopMode: "terminate", // worker を terminate して確実に止めるモード
            }),
        [],
    );
    const [status, setStatus] = useState<"idle" | "init" | "searching" | "error">("idle");
    const [bestmove, setBestmove] = useState<string | null>(null);
    const [logs, setLogs] = useState<string[]>([]);

    useEffect(() => {
        let handle: SearchHandle | null = null;
        const unsubscribe = engine.subscribe((event) => {
            setLogs((prev) => [...prev.slice(-(MAX_LOGS - 1)), formatEvent(event)]);
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
                handle.cancel().catch(() => undefined);
            }
            unsubscribe();
            engine.dispose().catch(() => undefined);
        };
    }, [engine]);

    return (
        <main className="app">
            <h1>Engine wiring (Web / Wasm)</h1>
            <p className="status">
                status: {status} {bestmove ? `| bestmove: ${bestmove}` : ""}
            </p>
            <section className="logs">
                <h2>Events (wasm worker)</h2>
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
