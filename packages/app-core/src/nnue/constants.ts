/**
 * NNUE ファイル管理の定数
 */

/**
 * NNUE ファイルの最大サイズ（バイト）
 * 悪意のあるファイルや誤アップロード対策
 *
 * 現在の一般的な NNUE サイズ:
 * - HalfKA 1024: ~72MB
 * - LayerStacks: ~100MB
 *
 * 150MB は将来の大型モデルに余裕を持たせた値
 */
export const NNUE_MAX_SIZE_BYTES = 150 * 1024 * 1024; // 150MB

/**
 * 進捗通知のスロットリング間隔（ミリ秒）
 */
export const NNUE_PROGRESS_THROTTLE_MS = 100;

/**
 * IndexedDB データベース名
 */
export const NNUE_DB_NAME = "shogi-nnue-storage";

/**
 * IndexedDB バージョン
 */
export const NNUE_DB_VERSION = 1;

/**
 * NNUE フォーマット検出に必要なヘッダサイズ（バイト）
 */
export const NNUE_HEADER_SIZE = 1024;
