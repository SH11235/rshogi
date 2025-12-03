import {
    createMockEngineClient,
    type EngineClient,
    type EngineEvent,
    type EngineEventHandler,
    type EngineInitOptions,
    type SearchHandle,
    type SearchParams,
} from "@shogi/engine-client";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";

type InvokeFn = typeof tauriInvoke;
type ListenFn = typeof tauriListen;

interface TauriIpc {
    invoke: InvokeFn;
    listen: ListenFn;
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
}

const DEFAULT_EVENT_NAME = "engine://event";

export function createTauriEngineClient(options: TauriEngineClientOptions = {}): EngineClient {
    const {
        ipc: ipcOverrides,
        useMockOnError = true,
        eventName = DEFAULT_EVENT_NAME,
        ...initDefaults
    } = options;

    const mock = createMockEngineClient();
    const listeners = new Set<EngineEventHandler>();
    const mockSubscriptions = new Map<EngineEventHandler, () => void>();
    const ipc: TauriIpc = {
        invoke: ipcOverrides?.invoke ?? tauriInvoke,
        listen: ipcOverrides?.listen ?? tauriListen,
    };

    let usingMock = false;
    let unlisten: UnlistenFn | null = null;

    const emit = (event: EngineEvent) => {
        listeners.forEach((handler) => handler(event));
    };

    const attachListenersToMock = () => {
        listeners.forEach((handler) => {
            if (mockSubscriptions.has(handler)) return;
            mockSubscriptions.set(handler, mock.subscribe(handler));
        });
    };

    const detachMockSubscriptions = () => {
        mockSubscriptions.forEach((unsubscribe) => unsubscribe());
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
            unlisten = await ipc.listen<EngineEvent>(eventName, (evt) => emit(evt.payload));
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
                },
                () => mock.init(mergedInitOpts),
            );
        },
        async loadPosition(sfen, moves) {
            return runOrMock(
                () => ipc.invoke("engine_position", { sfen, moves }),
                () => mock.loadPosition(sfen, moves),
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
                () => ipc.invoke("engine_option", { name, value }),
                () => mock.setOption(name, value),
            );
        },
        subscribe(handler) {
            listeners.add(handler);
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
        },
    };
}
