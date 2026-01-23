import {
    createMockEngineClient,
    type EngineClient,
    type EngineEvent,
    type EngineEventHandler,
    type EngineInitOptions,
    type LoadPositionOptions,
    type SearchHandle,
    type SearchParams,
    type ThreadInfo,
} from "@shogi/engine-client";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";

// NNUE ストレージ
export {
    calculateNnueHash,
    createTauriNnueStorage,
    getNnuePath,
    importNnue,
    importNnueFromPath,
    type TauriNnueStorageOptions,
} from "./nnue-storage";

export type InvokeFn = typeof tauriInvoke;
export type ListenFn = typeof tauriListen;

export interface TauriIpc {
    invoke: InvokeFn;
    listen: ListenFn;
}
export interface LegalMovesParams {
    sfen: string;
    moves?: string[];
    passRights?: { sente: number; gote: number };
    ipc?: Partial<TauriIpc>;
}

export interface TauriEngineClientOptions extends EngineInitOptions {
    /**
     * IPC 実装を差し替える場合に指定 (テスト用)。
     */
    ipc?: Partial<TauriIpc>;
    /**
     * IPC エラー時にモックへフォールバックするか。false なら例外をそのまま投げる。
     */
    useMockOnError?: boolean;
    /**
     * エンジンイベントのチャンネル名。デフォルトは `engine://event` を想定。
     */
    eventName?: string;
    /**
     * コンソールにデバッグログを出すか。
     */
    debug?: boolean;
}

const DEFAULT_EVENT_NAME = "engine://event";

export function createTauriEngineClient(options: TauriEngineClientOptions = {}): EngineClient {
    const {
        ipc: ipcOverrides,
        useMockOnError = true,
        eventName = DEFAULT_EVENT_NAME,
        ...initDefaults
    } = options;
    const debug = options.debug ?? false;

    const mock = createMockEngineClient();
    const listeners = new Set<EngineEventHandler>();
    const mockSubscriptions = new Map<EngineEventHandler, () => void>();
    const ipc: TauriIpc = {
        invoke: ipcOverrides?.invoke ?? tauriInvoke,
        listen: ipcOverrides?.listen ?? tauriListen,
    };

    let usingMock = false;
    let unlisten: UnlistenFn | null = null;
    let cachedThreadInfo: ThreadInfo | null = null;

    const defaultThreadInfo: ThreadInfo = {
        activeThreads: 1,
        maxThreads: 1,
        threadedAvailable: true,
        hardwareConcurrency: 1,
    };

    const fetchThreadInfo = async (): Promise<ThreadInfo> => {
        try {
            const response = await ipc.invoke<{
                activeThreads: number;
                maxThreads: number;
                threadedAvailable: boolean;
                hardwareConcurrency: number;
            }>("engine_thread_info");
            return {
                activeThreads: response.activeThreads,
                maxThreads: response.maxThreads,
                threadedAvailable: response.threadedAvailable,
                hardwareConcurrency: response.hardwareConcurrency,
            };
        } catch (error) {
            if (debug) {
                console.warn("[engine-tauri] Failed to fetch thread info, using defaults:", error);
            }
            return defaultThreadInfo;
        }
    };

    const emit = (event: EngineEvent) => {
        if (debug) {
            console.info("[engine-tauri] emit", { listenerCount: listeners.size, event });
        }
        for (const handler of listeners) {
            handler(event);
        }
    };

    const attachListenersToMock = () => {
        for (const handler of listeners) {
            if (mockSubscriptions.has(handler)) continue;
            mockSubscriptions.set(handler, mock.subscribe(handler));
        }
    };

    const detachMockSubscriptions = () => {
        for (const unsubscribe of mockSubscriptions.values()) {
            unsubscribe();
        }
        mockSubscriptions.clear();
    };

    const switchToMock = async () => {
        usingMock = true;
        if (unlisten) {
            try {
                unlisten();
            } catch {
                // ignore unlisten errors during fallback
            }
            unlisten = null;
        }
        attachListenersToMock();
    };

    const ensureEventSubscription = async () => {
        if (usingMock || unlisten) return;
        try {
            if (debug) {
                console.info("[engine-tauri] subscribing to", eventName);
            }
            unlisten = await ipc.listen<EngineEvent>(eventName, (evt) => {
                if (debug) {
                    console.info("[engine-tauri] event", evt.payload);
                }
                emit(evt.payload);
            });
        } catch (error) {
            console.error("Failed to subscribe to engine events:", error);
            if (useMockOnError === false) throw error;
            await switchToMock();
        }
    };

    const handleIpcError = async (error: unknown) => {
        if (useMockOnError === false) throw error;
        await switchToMock();
    };

    const runOrMock = async <T>(fn: () => Promise<T>, mockFn: () => Promise<T>): Promise<T> => {
        if (usingMock) return mockFn();
        try {
            const result = await fn();
            return result;
        } catch (error) {
            await handleIpcError(error);
            return mockFn();
        }
    };

    return {
        async init(initOpts) {
            const mergedInitOpts =
                initOpts ?? (Object.keys(initDefaults).length > 0 ? initDefaults : undefined);
            return runOrMock(
                async () => {
                    await ipc.invoke("engine_init", { opts: mergedInitOpts });
                    await ensureEventSubscription();
                    // Fetch and cache thread info after init
                    cachedThreadInfo = await fetchThreadInfo();
                },
                () => mock.init(mergedInitOpts),
            );
        },
        async loadPosition(sfen, moves, options?: LoadPositionOptions) {
            return runOrMock(
                () =>
                    ipc.invoke("engine_position", {
                        sfen,
                        moves,
                        pass_rights: options?.passRights,
                    }),
                () => mock.loadPosition(sfen, moves, options),
            );
        },
        async search(params: SearchParams): Promise<SearchHandle> {
            if (usingMock) return mock.search(params);

            try {
                await ensureEventSubscription();
                await ipc.invoke("engine_search", { params });
                return {
                    cancel: async () => {
                        await ipc.invoke("engine_stop").catch(() => undefined);
                    },
                };
            } catch (error) {
                await handleIpcError(error);
                return mock.search(params);
            }
        },
        async stop() {
            return runOrMock(
                () => ipc.invoke("engine_stop"),
                () => mock.stop(),
            );
        },
        async setOption(name, value) {
            return runOrMock(
                async () => {
                    await ipc.invoke("engine_option", { name, value });
                    // Update cache if Threads option was changed
                    if (name === "Threads") {
                        cachedThreadInfo = await fetchThreadInfo();
                    }
                },
                () => mock.setOption(name, value),
            );
        },
        subscribe(handler) {
            listeners.add(handler);
            if (debug) {
                console.info("[engine-tauri] subscribe", { listenerCount: listeners.size });
            }
            if (usingMock && !mockSubscriptions.has(handler)) {
                mockSubscriptions.set(handler, mock.subscribe(handler));
            }

            return () => {
                listeners.delete(handler);
                const unsubscribe = mockSubscriptions.get(handler);
                if (unsubscribe) {
                    unsubscribe();
                    mockSubscriptions.delete(handler);
                }
                if (debug) {
                    console.info("[engine-tauri] unsubscribe", { listenerCount: listeners.size });
                }
            };
        },
        async dispose() {
            await runOrMock(
                async () => {
                    await ipc.invoke("engine_stop").catch(() => undefined);
                    if (unlisten) {
                        try {
                            unlisten();
                        } catch {
                            // ignore unlisten errors during dispose
                        }
                        unlisten = null;
                    }
                },
                () => mock.dispose(),
            );
            detachMockSubscriptions();
            listeners.clear();
            cachedThreadInfo = null;
        },
        getThreadInfo(): ThreadInfo {
            return cachedThreadInfo ?? defaultThreadInfo;
        },
        async reset(): Promise<void> {
            // モック使用中の場合はモックをリセット
            if (usingMock) {
                await mock.dispose();
                usingMock = false;
                detachMockSubscriptions();
            }

            // 進行中の検索を停止
            try {
                await ipc.invoke("engine_stop");
            } catch {
                // ignore stop errors during reset
            }

            // イベントリスナーを解除（再登録は init 後に行う）
            if (unlisten) {
                try {
                    unlisten();
                } catch {
                    // ignore unlisten errors during reset
                }
                unlisten = null;
            }

            // キャッシュをクリア
            cachedThreadInfo = null;

            // Note: init() は呼び出し側が明示的に呼ぶ必要がある
        },
    };
}

export async function getLegalMoves(params: LegalMovesParams): Promise<string[]> {
    const ipc = {
        invoke: params.ipc?.invoke ?? tauriInvoke,
    };
    try {
        const result = await ipc.invoke<string[]>("engine_legal_moves", {
            sfen: params.sfen,
            moves: params.moves,
            pass_rights: params.passRights,
        });
        return result;
    } catch (error) {
        console.error("Failed to get legal moves:", error);
        return [];
    }
}
