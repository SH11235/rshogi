import type { EngineClient, EngineInfoEvent, SearchHandle } from "@shogi/engine-client";
import { useCallback, useEffect, useRef, useState } from "react";

/**
 * 解析ジョブ
 */
export interface AnalysisJob {
    ply: number;
    sfen: string;
    moves: string[];
    timeMs: number;
    depth: number;
}

/**
 * 一括解析の進捗情報
 */
interface BatchAnalysisProgress {
    completed: number;
    total: number;
    inProgress: number[]; // 解析中の手番号
}

/**
 * エンジンプールの設定
 */
interface UseEnginePoolOptions {
    /** エンジンクライアントを生成するファクトリ関数 */
    createClient: () => EngineClient;
    /** ワーカー数 */
    workerCount: number;
    /** 進捗更新時のコールバック */
    onProgress?: (progress: BatchAnalysisProgress) => void;
    /** 解析結果のコールバック */
    onResult?: (ply: number, event: EngineInfoEvent) => void;
    /** 全ジョブ完了時のコールバック */
    onComplete?: () => void;
    /** エラー時のコールバック */
    onError?: (ply: number, error: Error) => void;
}

/**
 * エンジンプールのハンドル
 */
interface EnginePoolHandle {
    /** 実行中かどうか */
    isRunning: boolean;
    /** 現在の進捗 */
    progress: BatchAnalysisProgress | null;
    /** 一括解析を開始する */
    start: (jobs: AnalysisJob[]) => void;
    /** 一括解析をキャンセルする */
    cancel: () => Promise<void>;
    /** プールを破棄する */
    dispose: () => Promise<void>;
}

interface EngineWorker {
    client: EngineClient;
    handle: SearchHandle | null;
    currentPly: number | null;
    subscription: (() => void) | null;
}

/**
 * エンジンプールを管理するフック
 * 複数のエンジンインスタンスを並列で使用して一括解析を行う
 */
export function useEnginePool(options: UseEnginePoolOptions): EnginePoolHandle {
    const { createClient, workerCount, onProgress, onResult, onComplete, onError } = options;

    const [isRunning, setIsRunning] = useState(false);
    const [progress, setProgress] = useState<BatchAnalysisProgress | null>(null);

    // マウント状態を追跡（Strict Mode対応）
    const mountedRef = useRef(true);

    // 内部状態をrefで管理（レンダリングに依存しない）
    const stateRef = useRef<{
        workers: EngineWorker[];
        jobQueue: AnalysisJob[];
        completed: number;
        total: number;
        inProgress: Set<number>;
        cancelled: boolean;
        initialized: boolean;
    }>({
        workers: [],
        jobQueue: [],
        completed: 0,
        total: 0,
        inProgress: new Set(),
        cancelled: false,
        initialized: false,
    });

    // コールバックをrefで保持（依存配列の問題を回避）
    const callbacksRef = useRef({ onProgress, onResult, onComplete, onError });
    useEffect(() => {
        callbacksRef.current = { onProgress, onResult, onComplete, onError };
    }, [onProgress, onResult, onComplete, onError]);

    // 進捗を更新する
    const updateProgress = useCallback(() => {
        const state = stateRef.current;
        const newProgress: BatchAnalysisProgress = {
            completed: state.completed,
            total: state.total,
            inProgress: Array.from(state.inProgress),
        };
        setProgress(newProgress);
        callbacksRef.current.onProgress?.(newProgress);
    }, []);

    // 次のジョブを取得して実行する
    // Note: JavaScriptはシングルスレッドのため、複数ワーカーが同時にこの関数を呼び出しても
    // jobQueue.shift() は安全にアトミックに実行される
    const processNextJob = useCallback(
        async (worker: EngineWorker) => {
            const state = stateRef.current;

            // キャンセルされた場合は処理しない
            if (state.cancelled) {
                return;
            }

            // キューからジョブを取得（シングルスレッドのため競合なし）
            const job = state.jobQueue.shift();
            if (!job) {
                // ジョブがない場合、全ワーカーがアイドルかチェック
                const allIdle = state.workers.every((w) => w.currentPly === null);
                if (allIdle && state.completed === state.total) {
                    setIsRunning(false);
                    callbacksRef.current.onComplete?.();
                }
                return;
            }

            // ジョブを実行
            worker.currentPly = job.ply;
            state.inProgress.add(job.ply);
            updateProgress();

            try {
                // 局面を読み込み
                await worker.client.loadPosition(job.sfen, job.moves);

                // 既存のサブスクリプションを解除
                if (worker.subscription) {
                    worker.subscription();
                    worker.subscription = null;
                }

                // イベントを購読
                worker.subscription = worker.client.subscribe((event) => {
                    if (state.cancelled) return;

                    if (event.type === "info") {
                        // 評価値が含まれている場合のみコールバック
                        if (event.scoreCp !== undefined || event.scoreMate !== undefined) {
                            callbacksRef.current.onResult?.(job.ply, event);
                        }
                    }

                    if (event.type === "bestmove") {
                        // 解析完了
                        worker.handle = null;
                        worker.currentPly = null;
                        state.inProgress.delete(job.ply);
                        state.completed++;
                        updateProgress();

                        // 次のジョブを処理
                        void processNextJob(worker);
                    }

                    if (event.type === "error") {
                        callbacksRef.current.onError?.(job.ply, new Error(event.message));
                        worker.handle = null;
                        worker.currentPly = null;
                        state.inProgress.delete(job.ply);
                        state.completed++;
                        updateProgress();

                        // エラーでも次のジョブを処理
                        void processNextJob(worker);
                    }
                });

                // 探索開始
                worker.handle = await worker.client.search({
                    limits: {
                        movetimeMs: job.timeMs,
                        maxDepth: job.depth,
                    },
                    ponder: false,
                });
            } catch (error) {
                callbacksRef.current.onError?.(job.ply, error as Error);
                worker.handle = null;
                worker.currentPly = null;
                state.inProgress.delete(job.ply);
                state.completed++;
                updateProgress();

                // エラーでも次のジョブを処理
                void processNextJob(worker);
            }
        },
        [updateProgress],
    );

    // ワーカーを初期化する
    const initializeWorkers = useCallback(async () => {
        const state = stateRef.current;
        if (state.initialized) return;

        const workers: EngineWorker[] = [];
        for (let i = 0; i < workerCount; i++) {
            try {
                const client = createClient();
                await client.init();
                workers.push({
                    client,
                    handle: null,
                    currentPly: null,
                    subscription: null,
                });
            } catch (error) {
                console.error(`Failed to initialize worker ${i}:`, error);
            }
        }

        // 初期化に成功したワーカーが1つもない場合はエラー
        if (workers.length === 0) {
            const error = new Error("No workers could be initialized");
            callbacksRef.current.onError?.(0, error);
            // アンマウント後は状態を更新しない
            if (mountedRef.current) {
                setIsRunning(false);
            }
            return;
        }

        // 要求された数より少ないワーカーしか初期化できなかった場合は警告
        if (workers.length < workerCount) {
            console.warn(
                `Only ${workers.length}/${workerCount} workers initialized. ` +
                    "Analysis will continue with fewer parallel workers.",
            );
        }

        state.workers = workers;
        state.initialized = true;
    }, [createClient, workerCount]);

    // 一括解析を開始する
    const start = useCallback(
        (jobs: AnalysisJob[]) => {
            const state = stateRef.current;

            // 状態をリセット
            state.jobQueue = [...jobs];
            state.completed = 0;
            state.total = jobs.length;
            state.inProgress.clear();
            state.cancelled = false;

            setIsRunning(true);
            updateProgress();

            // 遅延初期化してジョブを開始
            void (async () => {
                await initializeWorkers();

                // 各ワーカーにジョブを割り当て
                for (const worker of state.workers) {
                    if (!state.cancelled) {
                        void processNextJob(worker);
                    }
                }
            })();
        },
        [initializeWorkers, processNextJob, updateProgress],
    );

    // 一括解析をキャンセルする
    const cancel = useCallback(async () => {
        const state = stateRef.current;
        state.cancelled = true;
        state.jobQueue = [];

        // 全ワーカーを停止
        const stopPromises = state.workers.map(async (worker) => {
            if (worker.handle) {
                try {
                    await worker.handle.cancel();
                } catch {
                    // 無視
                }
                worker.handle = null;
            }
            if (worker.subscription) {
                worker.subscription();
                worker.subscription = null;
            }
            worker.currentPly = null;
        });

        await Promise.all(stopPromises);

        state.inProgress.clear();

        // アンマウント後は状態を更新しない
        if (mountedRef.current) {
            setIsRunning(false);
            setProgress(null);
        }
    }, []);

    // プールを破棄する
    const dispose = useCallback(async () => {
        await cancel();

        const state = stateRef.current;
        const disposePromises = state.workers.map(async (worker) => {
            try {
                await worker.client.dispose();
            } catch {
                // 無視
            }
        });

        await Promise.all(disposePromises);
        state.workers = [];
        state.initialized = false;
    }, [cancel]);

    // マウント時の初期化とアンマウント時のクリーンアップ
    // React 18 Strict Mode では2回実行されるため、mountedRef で状態を追跡
    useEffect(() => {
        mountedRef.current = true;

        return () => {
            mountedRef.current = false;
            // 非同期の dispose を実行（完了を待たない）
            // Strict Mode では2回目のマウント前にクリーンアップが実行される
            void dispose();
        };
    }, [dispose]);

    return {
        isRunning,
        progress,
        start,
        cancel,
        dispose,
    };
}
