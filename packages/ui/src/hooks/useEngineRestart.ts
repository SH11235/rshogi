import type { EngineClient, EngineInitOptions } from "@shogi/engine-client";
import { useCallback, useState } from "react";

export interface UseEngineRestartReturn {
    /** エンジン再起動中かどうか */
    isRestarting: boolean;
    /** 再起動エラー */
    error: Error | null;
    /** エンジンを再起動する */
    restart: (initOptions?: EngineInitOptions) => Promise<void>;
    /** エラーをクリア */
    clearError: () => void;
}

export interface UseEngineRestartOptions {
    /** エンジンクライアント */
    engine: EngineClient | null;
    /** 再起動完了後のコールバック */
    onRestarted?: () => void;
    /** 再起動エラー時のコールバック */
    onError?: (error: Error) => void;
}

/**
 * エンジン再起動を管理するフック
 *
 * reset() → init() のシーケンスを実行し、状態を管理する。
 */
export function useEngineRestart({
    engine,
    onRestarted,
    onError,
}: UseEngineRestartOptions): UseEngineRestartReturn {
    const [isRestarting, setIsRestarting] = useState(false);
    const [error, setError] = useState<Error | null>(null);

    const restart = useCallback(
        async (initOptions?: EngineInitOptions) => {
            if (!engine) {
                const err = new Error("エンジンが設定されていません");
                setError(err);
                onError?.(err);
                return;
            }

            setIsRestarting(true);
            setError(null);

            try {
                // reset() が存在する場合は呼び出す
                if (engine.reset) {
                    await engine.reset();
                }
                // init() で再初期化
                await engine.init(initOptions);
                onRestarted?.();
            } catch (e) {
                const err = e instanceof Error ? e : new Error(String(e));
                setError(err);
                onError?.(err);
            } finally {
                setIsRestarting(false);
            }
        },
        [engine, onRestarted, onError],
    );

    const clearError = useCallback(() => {
        setError(null);
    }, []);

    return {
        isRestarting,
        error,
        restart,
        clearError,
    };
}
