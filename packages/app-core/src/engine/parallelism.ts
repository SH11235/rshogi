/**
 * 並列解析の設定を管理するユーティリティ
 */

/**
 * 並列処理の設定情報
 */
export interface ParallelismConfig {
    /** 絶対上限（Wasm制約: 4） */
    maxWorkers: number;
    /** 検出されたハードウェア並列数 */
    detectedConcurrency: number;
    /** 推奨ワーカー数 */
    recommendedWorkers: number;
}

/**
 * ハードウェアの並列処理能力を検出し、推奨設定を返す
 */
export function detectParallelism(): ParallelismConfig {
    // navigator.hardwareConcurrency の値を検証し、異常値を防ぐ
    // （カスタムブラウザや開発者ツールで不正な値が設定される可能性があるため）
    const rawConcurrency =
        typeof navigator !== "undefined" && typeof navigator.hardwareConcurrency === "number"
            ? navigator.hardwareConcurrency
            : 1;
    const hardwareConcurrency = Math.max(1, Math.min(rawConcurrency, 128));

    // 推奨: コア数の半分（最低1、最大4）
    // Wasm の MAX_WASM_THREADS = 4 制限を考慮
    const recommended = Math.max(1, Math.min(4, Math.floor(hardwareConcurrency / 2)));

    return {
        maxWorkers: 4, // Wasm MAX_WASM_THREADS 制限
        detectedConcurrency: hardwareConcurrency,
        recommendedWorkers: recommended,
    };
}

/**
 * ユーザー設定から実際のワーカー数を解決する
 * @param userSetting ユーザー設定（0=自動検出）
 * @returns 実際に使用するワーカー数
 */
export function resolveWorkerCount(userSetting: number): number {
    const config = detectParallelism();
    if (userSetting === 0) {
        return config.recommendedWorkers;
    }
    return Math.min(userSetting, config.maxWorkers);
}
