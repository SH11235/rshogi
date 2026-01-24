/**
 * NNUE エラーコード
 */
export type NnueErrorCode =
    | "NNUE_NOT_FOUND"
    | "NNUE_INVALID_FORMAT"
    | "NNUE_INCOMPATIBLE"
    | "NNUE_ALREADY_LOADED"
    | "NNUE_DOWNLOAD_FAILED"
    | "NNUE_DOWNLOAD_IN_PROGRESS"
    | "NNUE_STORAGE_FULL"
    | "NNUE_SIZE_MISMATCH"
    | "NNUE_HASH_MISMATCH"
    | "NNUE_STORAGE_FAILED"
    | "NNUE_DELETE_FAILED"
    | "NNUE_SIZE_EXCEEDED";

/**
 * NNUE エラー
 */
export class NnueError extends Error {
    readonly code: NnueErrorCode;
    readonly cause?: unknown;

    constructor(code: NnueErrorCode, message: string, cause?: unknown) {
        super(message);
        this.name = "NnueError";
        this.code = code;
        this.cause = cause;
    }
}

/**
 * エラーコードに対応するユーザー向けメッセージ（日本語）
 */
export const NNUE_ERROR_MESSAGES: Record<NnueErrorCode, string> = {
    NNUE_NOT_FOUND: "NNUE ファイルが見つかりません",
    NNUE_INVALID_FORMAT: "このファイルは NNUE 形式ではありません",
    NNUE_INCOMPATIBLE: "このエンジンは指定された NNUE 形式に対応していません",
    NNUE_ALREADY_LOADED: "NNUE を変更するにはエンジンを再起動してください",
    NNUE_DOWNLOAD_FAILED: "ダウンロードに失敗しました。ネットワークを確認してください",
    NNUE_DOWNLOAD_IN_PROGRESS: "このファイルは既にダウンロード中です",
    NNUE_STORAGE_FULL: "ストレージ容量が不足しています。不要な NNUE を削除してください",
    NNUE_SIZE_MISMATCH: "ダウンロードしたファイルサイズが一致しません",
    NNUE_HASH_MISMATCH: "ファイル検証に失敗しました（破損の可能性）",
    NNUE_STORAGE_FAILED: "保存に失敗しました（容量不足の可能性）",
    NNUE_DELETE_FAILED: "削除に失敗しました",
    NNUE_SIZE_EXCEEDED: "ファイルサイズが上限を超えています",
};

/**
 * ユーザー向けエラーメッセージを取得
 */
export function getNnueErrorMessage(error: NnueError): string {
    return NNUE_ERROR_MESSAGES[error.code] ?? error.message;
}
