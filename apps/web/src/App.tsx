import { createWasmEngineClient } from "@shogi/engine-wasm";
import { EngineControlPanel, ShogiMatch } from "@shogi/ui";

const createEngineClient = () =>
    createWasmEngineClient({
        stopMode: "terminate",
    });

const engineOptions = [
    { id: "wasm-a", label: "内蔵エンジン（スロットA）", createClient: createEngineClient },
    { id: "wasm-b", label: "内蔵エンジン（スロットB）", createClient: createEngineClient },
];

const panelEngine = createEngineClient();

function App() {
    return (
        <main className="mx-auto flex max-w-[1100px] flex-col gap-[18px] px-5 pb-[72px] pt-10">
            <section className="rounded-2xl border border-[hsl(var(--border))] bg-gradient-to-br from-[rgba(255,138,76,0.12)] to-[rgba(255,209,126,0.18)] px-[22px] py-5 shadow-[0_18px_36px_rgba(0,0,0,0.12)]">
                <p className="m-0 text-xs font-bold uppercase tracking-[0.14em] text-[hsl(var(--accent))]">
                    Web / Wasm
                </p>
                <h1 className="mb-1 mt-2 text-[30px] tracking-[-0.02em]">Shogi Playground</h1>
                <p className="m-0 text-[15px] leading-relaxed text-[hsl(var(--muted-foreground))]">
                    盤 UI とエンジン操作をひとまとめにしたデバッグページ。Wasm エンジンを startpos
                    から動かしつつ、 手入力や棋譜入出力を試せます。
                </p>
                <p className="mb-0 mt-2 text-xs text-[hsl(var(--muted-foreground))]">
                    同じ UI を Desktop でも再利用できるよう構成しています。
                </p>
            </section>

            <div className="flex flex-col gap-[14px]">
                <ShogiMatch engineOptions={engineOptions} />
                <EngineControlPanel engine={panelEngine} />
            </div>
        </main>
    );
}

export default App;
