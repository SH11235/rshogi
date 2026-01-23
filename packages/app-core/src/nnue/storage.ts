/**
 * NNUE ストレージインターフェース
 *
 * Web (IndexedDB) と Desktop (ファイルシステム) で共通のインターフェースを提供
 */

import type { NnueMeta } from "./types";

/**
 * NnueStorage の capability
 *
 * 各プラットフォームでサポートする機能を示す
 */
export interface NnueStorageCapabilities {
    /** File オブジェクトからの import をサポートするか（drag&drop, input[type=file]） */
    supportsFileImport: boolean;
    /** ファイルパスからの import をサポートするか（Tauri ダイアログ経由） */
    supportsPathImport: boolean;
    /** load/loadStream をサポートするか（Web: true, Desktop: false） */
    supportsLoad: boolean;
}

/**
 * NNUE ストレージインターフェース
 */
export interface NnueStorage {
    /** ストレージの capability */
    readonly capabilities: NnueStorageCapabilities;
    /**
     * NNUE ファイルを保存
     * @param id 識別子
     * @param data ファイルデータ
     * @param meta メタデータ
     */
    save(id: string, data: Blob | Uint8Array, meta: NnueMeta): Promise<void>;

    /**
     * NNUE ファイルを読み込み（バイト配列として）
     * capabilities.supportsLoad === true の場合のみ利用可能
     * @param id 識別子
     */
    load?(id: string): Promise<Uint8Array>;

    /**
     * NNUE ファイルを読み込み（ストリームとして）
     * capabilities.supportsLoad === true の場合のみ利用可能
     * @param id 識別子
     */
    loadStream?(id: string): Promise<ReadableStream<Uint8Array>>;

    /**
     * NNUE ファイルを削除
     * @param id 識別子
     */
    delete(id: string): Promise<void>;

    /**
     * 全 NNUE メタデータを取得
     */
    listMeta(): Promise<NnueMeta[]>;

    /**
     * 特定の NNUE メタデータを取得
     * @param id 識別子
     */
    getMeta(id: string): Promise<NnueMeta | null>;

    /**
     * メタデータのみ更新
     * @param id 識別子
     * @param partial 更新するフィールド
     */
    updateMeta(id: string, partial: Partial<NnueMeta>): Promise<void>;

    /**
     * ストレージ使用量を取得
     */
    getUsage(): Promise<{ used: number; quota?: number }>;

    /**
     * コンテンツハッシュで検索
     * @param hash SHA-256 ハッシュ
     */
    listByContentHash(hash: string): Promise<NnueMeta[]>;

    /**
     * プリセットキーで検索
     * @param presetKey プリセットキー
     */
    listByPresetKey(presetKey: string): Promise<NnueMeta[]>;

    /**
     * ファイルパスから NNUE をインポート（Desktop 専用）
     * @param srcPath ソースファイルのパス
     * @param displayName 表示名（省略時はファイル名）
     */
    importFromPath?(srcPath: string, displayName?: string): Promise<NnueMeta>;
}
