/**
 * NNUE マネージャーインターフェース
 *
 * NNUE ファイルのインポート、ダウンロード、検証を管理する
 */

import type { NnueDownloadProgress, NnueFormat, NnueMeta, PresetUpdate } from "./types";

/**
 * NNUE マネージャーインターフェース
 */
interface NnueManager {
    /**
     * ファイルから NNUE をインポート
     * @param file ファイルオブジェクト
     */
    importFromFile(file: File): Promise<NnueMeta>;

    /**
     * URL から NNUE をダウンロード・保存
     * @param url ダウンロード URL
     * @param displayName 表示名（省略時は URL から推測）
     */
    importFromUrl(url: string, displayName?: string): Promise<NnueMeta>;

    /**
     * プリセット NNUE をダウンロード
     * @param presetKey manifest.json の presetKey と対応
     */
    downloadPreset(presetKey: string): Promise<NnueMeta>;

    /**
     * NNUE を削除
     * @param id NnueMeta.id（UUID）
     */
    delete(id: string): Promise<void>;

    /**
     * 全 NNUE 一覧を取得
     */
    list(): Promise<NnueMeta[]>;

    /**
     * 特定の NNUE メタデータを取得
     * @param id NnueMeta.id（UUID）
     */
    get(id: string): Promise<NnueMeta | null>;

    /**
     * NNUE フォーマットを検証
     * @param id NnueMeta.id（UUID）
     * @returns 互換性があれば NnueFormat、なければエラー
     */
    validate(id: string): Promise<NnueFormat>;

    /**
     * NNUE をエンジンで検証（実際にロード可能か確認）
     * @param id NnueMeta.id（UUID）
     */
    verifyWithEngine(id: string): Promise<void>;

    /**
     * ダウンロード進捗を購読
     * @param handler 進捗ハンドラ
     * @returns 購読解除関数
     */
    onProgress(handler: (progress: NnueDownloadProgress) => void): () => void;

    /**
     * プリセット更新をチェック
     */
    checkUpdates(): Promise<PresetUpdate[]>;

    /**
     * 重複ファイルの存在確認
     * @param hash SHA-256 ハッシュ
     */
    hasDuplicate(hash: string): Promise<boolean>;
}
