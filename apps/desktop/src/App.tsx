import { createTauriEngineClient, getLegalMoves } from "@shogi/engine-tauri";
import { EngineControlPanel, ShogiMatch } from "@shogi/ui";

const createEngineClient = () =>
    createTauriEngineClient({
        stopMode: "terminate",
        useMockOnError: false,
        debug: true,
    });

const engineOptions = [
    { id: "native", label: "内蔵エンジン", createClient: createEngineClient, kind: "internal" },
];

const panelEngine = createEngineClient();

function App() {
    return (
        <main className="mx-auto flex max-w-[1100px] flex-col gap-[18px] px-5 pb-[72px] pt-10">
            <section className="rounded-2xl border border-[hsl(var(--border))] bg-gradient-to-br from-[rgba(255,138,76,0.12)] to-[rgba(255,209,126,0.18)] px-[22px] py-5 shadow-[0_18px_36px_rgba(0,0,0,0.12)]">
                <p className="m-0 text-xs font-bold uppercase tracking-[0.14em] text-[hsl(var(--accent))]">
                    Desktop / Tauri
                </p>
                <h1 className="mb-1 mt-2 text-[30px] tracking-[-0.02em]">Shogi Playground</h1>
                <p className="m-0 text-[15px] leading-relaxed text-[hsl(var(--muted-foreground))]">
                    ネイティブエンジンへ IPC で init/loadPosition/search/setoption
                    を送るモーダルと盤 UI の統合デモ。 Web 版と共通の UI を利用しています。
                </p>
                <p className="mb-0 mt-2 text-xs text-[hsl(var(--muted-foreground))]">
                    StrictMode でも自動探索を避けるため、操作はすべて明示的ボタンで発火。
                </p>
            </section>

            <div className="flex flex-col gap-[14px]">
                <ShogiMatch
                    engineOptions={engineOptions}
                    fetchLegalMoves={(moves) => getLegalMoves({ sfen: "startpos", moves })}
                />
                <EngineControlPanel engine={panelEngine} />
            </div>
        </main>
    );
}

export default App;
