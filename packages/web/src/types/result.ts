/**
 * Result型 - エラーハンドリングのためのUnion型
 * RustのResult<T, E>に着想を得た実装
 */

export type Result<T, E = Error> =
    | { readonly success: true; readonly data: T }
    | { readonly success: false; readonly error: E };

/**
 * 成功結果を作成
 */
export const Ok = <T>(data: T): Result<T, never> =>
    ({
        success: true,
        data,
    }) as const;

/**
 * エラー結果を作成
 */
export const Err = <E>(error: E): Result<never, E> =>
    ({
        success: false,
        error,
    }) as const;

/**
 * Result型のユーティリティ関数
 */
export const ResultUtils = {
    /**
     * Resultが成功かどうかを判定
     */
    isOk: <T, E>(result: Result<T, E>): result is { success: true; data: T } => result.success,

    /**
     * Resultがエラーかどうかを判定
     */
    isErr: <T, E>(result: Result<T, E>): result is { success: false; error: E } => !result.success,

    /**
     * Resultをmapする（成功時のみ）
     */
    map: <T, U, E>(result: Result<T, E>, fn: (data: T) => U): Result<U, E> =>
        result.success ? Ok(fn(result.data)) : result,

    /**
     * ResultをflatMapする（成功時のみ）
     */
    flatMap: <T, U, E>(result: Result<T, E>, fn: (data: T) => Result<U, E>): Result<U, E> =>
        result.success ? fn(result.data) : result,

    /**
     * エラーをmapする（エラー時のみ）
     */
    mapError: <T, E, F>(result: Result<T, E>, fn: (error: E) => F): Result<T, F> =>
        result.success ? result : Err(fn(result.error)),

    /**
     * デフォルト値でunwrapする
     */
    unwrapOr: <T, E>(result: Result<T, E>, defaultValue: T): T =>
        result.success ? result.data : defaultValue,

    /**
     * PromiseをResult型に変換
     */
    fromPromise: async <T>(promise: Promise<T>): Promise<Result<T, Error>> => {
        try {
            const data = await promise;
            return Ok(data);
        } catch (error) {
            return Err(error instanceof Error ? error : new Error(String(error)));
        }
    },

    /**
     * 複数のResultを組み合わせる（すべて成功時のみ成功）
     */
    combine: <T extends readonly unknown[], E>(
        results: { [K in keyof T]: Result<T[K], E> },
    ): Result<T, E> => {
        const data: unknown[] = [];
        for (const result of results) {
            if (!result.success) {
                return result;
            }
            data.push(result.data);
        }
        return Ok(data as unknown as T);
    },
} as const;
