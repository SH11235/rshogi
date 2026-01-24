/**
 * NNUE ファイル管理のユーティリティ関数
 */

/**
 * NNUE ファイル用の一意な ID を生成する
 *
 * UUID v4 形式で生成される。
 * 将来的に ID 生成戦略を変更する際の影響範囲を最小化するため、
 * 直接 crypto.randomUUID() を呼び出さず、この関数を使用すること。
 */
export function generateNnueId(): string {
    return crypto.randomUUID();
}
