/**
 * 合法手のキャッシュを管理するクラス
 *
 * 局面の手数（ply）をキーとして合法手のセットをキャッシュし、
 * 同じ局面での重複計算を避けます。
 */
export class LegalMoveCache {
    private cache: { ply: number; moves: Set<string> } | null = null;

    /**
     * 指定された手数のキャッシュが存在するかチェックする
     *
     * @param ply - チェック対象の手数
     * @returns キャッシュが存在する場合は true
     */
    isCached(ply: number): boolean {
        return this.cache !== null && this.cache.ply === ply;
    }

    /**
     * キャッシュされた合法手のセットを取得する
     *
     * @returns キャッシュが存在する場合は合法手のセット、存在しない場合は null
     */
    getCached(): Set<string> | null {
        return this.cache?.moves ?? null;
    }

    /**
     * 合法手のセットをキャッシュに保存する
     *
     * @param ply - 手数
     * @param moves - 合法手のセット
     */
    set(ply: number, moves: Set<string>): void {
        this.cache = { ply, moves };
    }

    /**
     * キャッシュをクリアする
     */
    clear(): void {
        this.cache = null;
    }

    /**
     * 指定された手数の合法手を取得する（キャッシュ優先）
     *
     * @param ply - 現在の手数
     * @param resolver - 合法手を解決する非同期関数
     * @returns 合法手のセット
     *
     * @example
     * ```typescript
     * const cache = new LegalMoveCache();
     * const resolver = async (ply: number) => {
     *   // 合法手を計算...
     *   return ["7g7f", "3c3d"];
     * };
     *
     * const moves = await cache.getOrResolve(1, resolver);
     * // 初回は resolver が呼ばれる
     *
     * const cachedMoves = await cache.getOrResolve(1, resolver);
     * // 2回目はキャッシュが返される（resolver は呼ばれない）
     * ```
     */
    async getOrResolve(
        ply: number,
        resolver: (ply: number) => Promise<string[]>,
    ): Promise<Set<string>> {
        if (this.isCached(ply)) {
            const cached = this.getCached();
            if (!cached) {
                throw new Error("Cache should exist when isCached returns true");
            }
            return cached;
        }

        const list = await resolver(ply);
        const set = new Set(list);
        this.set(ply, set);
        return set;
    }
}
