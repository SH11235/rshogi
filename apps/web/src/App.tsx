import type { NnueStorage } from "@shogi/app-core";
import { createIndexedDBNnueStorage, createWasmEngineClient } from "@shogi/engine-wasm";
import type { EngineOption } from "@shogi/ui";
import { EngineControlPanel, NnueProvider, ShogiMatch, useDevMode } from "@shogi/ui";
import { useEffect, useState } from "react";

const resolveWasmThreads = () => {
    const fallback = import.meta.env.DEV ? 4 : 1;
    const raw = import.meta.env.VITE_WASM_THREADS;
    if (typeof raw !== "string" || raw.trim() === "") return fallback;
    const parsed = Number(raw);
    if (!Number.isFinite(parsed) || parsed < 1) return fallback;
    return Math.trunc(parsed);
};

const wasmThreads = resolveWasmThreads();

const createEngineClient = () =>
    createWasmEngineClient({
        stopMode: "terminate",
        defaultInitOptions: { threads: wasmThreads },
        logWarningsToConsole: true,
    });

const engineOptions: EngineOption[] = [
    { id: "wasm", label: "内蔵エンジン", createClient: createEngineClient, kind: "internal" },
];

const panelEngine = createEngineClient();

function App() {
    const isDevMode = useDevMode();
    const [nnueStorage, setNnueStorage] = useState<NnueStorage | null>(null);

    useEffect(() => {
        let cancelled = false;
        createIndexedDBNnueStorage()
            .then((storage) => {
                if (!cancelled) {
                    setNnueStorage(storage);
                }
            })
            .catch((error) => {
                console.error("Failed to initialize NNUE storage:", error);
            });
        return () => {
            cancelled = true;
        };
    }, []);

    const content = (
        <main className="mx-auto flex max-w-[1100px] flex-col gap-3 md:px-5">
            <ShogiMatch engineOptions={engineOptions} isDevMode={isDevMode} />
            {isDevMode && <EngineControlPanel engine={panelEngine} />}
        </main>
    );

    if (!nnueStorage) {
        return content;
    }

    return (
        <NnueProvider storage={nnueStorage} platform="web">
            {content}
        </NnueProvider>
    );
}

export default App;
