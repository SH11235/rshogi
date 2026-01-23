import {
    createTauriEngineClient,
    createTauriNnueStorage,
    getLegalMoves,
} from "@shogi/engine-tauri";
import type { EngineOption } from "@shogi/ui";
import { EngineControlPanel, NnueProvider, ShogiMatch } from "@shogi/ui";
import { open } from "@tauri-apps/plugin-dialog";

const createEngineClient = () =>
    createTauriEngineClient({
        stopMode: "terminate",
        useMockOnError: false,
        debug: true,
    });

const engineOptions: EngineOption[] = [
    { id: "native", label: "内蔵エンジン", createClient: createEngineClient, kind: "internal" },
];

const panelEngine = createEngineClient();

// Tauri版のストレージは同期的に初期化可能
const nnueStorage = createTauriNnueStorage();

// NNUE プリセット manifest.json の URL（環境変数で設定）
const nnueManifestUrl = import.meta.env.VITE_NNUE_MANIFEST_URL as string | undefined;

// NNUE ファイル選択ダイアログを開く
async function requestNnueFilePath(): Promise<string | null> {
    const result = await open({
        filters: [{ name: "NNUE Files", extensions: ["nnue"] }],
        multiple: false,
        directory: false,
    });
    // result は string | string[] | null
    if (typeof result === "string") {
        return result;
    }
    return null;
}

function App() {
    // デスクトップ版は常に開発者モードを有効化
    return (
        <NnueProvider storage={nnueStorage}>
            <main className="mx-auto flex max-w-[1100px] flex-col gap-3 md:px-5">
                <ShogiMatch
                    engineOptions={engineOptions}
                    fetchLegalMoves={(sfen, moves, options) =>
                        getLegalMoves({ sfen, moves, passRights: options?.passRights })
                    }
                    isDevMode={true}
                    manifestUrl={nnueManifestUrl}
                    onRequestNnueFilePath={requestNnueFilePath}
                />
                <EngineControlPanel engine={panelEngine} />
            </main>
        </NnueProvider>
    );
}

export default App;
