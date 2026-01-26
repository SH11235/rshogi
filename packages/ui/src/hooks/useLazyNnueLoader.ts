import {
    createPresetManager,
    NNUE_HEADER_SIZE,
    type NnueDownloadProgress,
    NnueError,
    type NnueSelection,
    type PresetManager,
} from "@shogi/app-core";
import { useCallback, useMemo, useState } from "react";
import { useNnueContextOptional } from "../providers/NnueContext";

interface UseLazyNnueLoaderReturn {
    /**
     * NNUE を解決する（必要ならダウンロード）
     * @param selection NNUE 選択状態
     * @returns 解決済みの nnueId、または null（駒得評価）
     */
    resolveNnue: (selection: NnueSelection) => Promise<string | null>;
    /** ダウンロード中かどうか */
    isDownloading: boolean;
    /** ダウンロード進捗 */
    downloadProgress: NnueDownloadProgress | null;
    /** ダウンロード中のプリセット表示名 */
    downloadingPresetName: string | null;
    /** エラー */
    error: NnueError | null;
    /** エラーをクリア */
    clearError: () => void;
}

interface UseLazyNnueLoaderOptions {
    /** manifest.json の URL */
    manifestUrl?: string;
}

/**
 * NNUE 遅延ロードフック
 *
 * NnueSelection を受け取り、実際の nnueId を返す。
 * プリセット指定の場合、IndexedDB に無ければダウンロードする。
 */
export function useLazyNnueLoader(options: UseLazyNnueLoaderOptions = {}): UseLazyNnueLoaderReturn {
    const { manifestUrl } = options;
    const context = useNnueContextOptional();
    const storage = context?.storage ?? null;
    const validateNnueHeader = context?.validateNnueHeader ?? null;

    const [isDownloading, setIsDownloading] = useState(false);
    const [downloadProgress, setDownloadProgress] = useState<NnueDownloadProgress | null>(null);
    const [downloadingPresetName, setDownloadingPresetName] = useState<string | null>(null);
    const [error, setError] = useState<NnueError | null>(null);

    // PresetManager インスタンスを作成
    const manager = useMemo<PresetManager | null>(() => {
        if (!manifestUrl || !storage) return null;
        return createPresetManager({
            manifestUrl,
            storage,
            onProgress: setDownloadProgress,
        });
    }, [manifestUrl, storage]);

    /**
     * NNUE を解決する
     *
     * 1. presetKey が null → nnueId をそのまま返す
     * 2. presetKey が設定されている:
     *    a. IndexedDB に該当 presetKey の NNUE があるか確認
     *    b. あれば → その id を返す
     *    c. なければ → ダウンロード → 保存された id を返す
     */
    const resolveNnue = useCallback(
        async (selection: NnueSelection): Promise<string | null> => {
            // プリセット指定でない場合は nnueId をそのまま返す
            if (!selection.presetKey) {
                return selection.nnueId;
            }

            // storage がない場合は nnueId にフォールバック
            if (!storage) {
                console.warn("NNUE storage is not available, falling back to nnueId");
                return selection.nnueId;
            }

            const presetKey = selection.presetKey;

            try {
                // IndexedDB に該当 presetKey の NNUE があるか確認
                const existing = await storage.listByPresetKey(presetKey);
                if (existing.length > 0) {
                    // 最新の作成日時のものを返す
                    const sorted = [...existing].sort((a, b) => b.createdAt - a.createdAt);
                    return sorted[0].id;
                }

                // manifest がない場合は nnueId にフォールバック
                if (!manager) {
                    console.warn("Preset manager is not available, falling back to nnueId");
                    return selection.nnueId;
                }

                // ダウンロードが必要
                setIsDownloading(true);
                setDownloadProgress(null);
                setError(null);

                // プリセット名を取得して表示用に設定
                const manifest = await manager.getManifest();
                const presetConfig = manifest.presets.find((p) => p.presetKey === presetKey);
                setDownloadingPresetName(presetConfig?.displayName ?? presetKey);

                // 重複チェック（ハッシュベース）
                const isDuplicate = await manager.isDuplicate(presetKey);
                if (isDuplicate && presetConfig) {
                    // 同じハッシュのファイルが既にある
                    const byHash = await storage.listByContentHash(presetConfig.sha256);
                    if (byHash.length > 0) {
                        setIsDownloading(false);
                        setDownloadProgress(null);
                        setDownloadingPresetName(null);
                        return byHash[0].id;
                    }
                }

                // ダウンロード実行
                const meta = await manager.download(presetKey);

                // ヘッダ検証（フォーマット情報の更新）
                if (validateNnueHeader && storage.capabilities.supportsLoad && storage.load) {
                    try {
                        const data = await storage.load(meta.id);
                        const header = data.subarray(
                            0,
                            Math.min(NNUE_HEADER_SIZE, data.byteLength),
                        );
                        const result = await validateNnueHeader(header);
                        if (result.isCompatible && result.format) {
                            await storage.updateMeta(meta.id, { format: result.format });
                        }
                    } catch {
                        // 検証失敗時はフォーマット情報を更新しない
                    }
                }

                setIsDownloading(false);
                setDownloadProgress(null);
                setDownloadingPresetName(null);
                return meta.id;
            } catch (e) {
                const err =
                    e instanceof NnueError
                        ? e
                        : new NnueError(
                              "NNUE_DOWNLOAD_FAILED",
                              `プリセット "${presetKey}" のダウンロードに失敗しました`,
                              e,
                          );
                setError(err);
                setIsDownloading(false);
                setDownloadProgress(null);
                setDownloadingPresetName(null);

                // ダウンロード失敗時は nnueId にフォールバック（あれば使用、なければ駒得評価）
                console.error("Failed to download preset NNUE:", err);
                return selection.nnueId;
            }
        },
        [manager, storage, validateNnueHeader],
    );

    const clearError = useCallback(() => {
        setError(null);
    }, []);

    return {
        resolveNnue,
        isDownloading,
        downloadProgress,
        downloadingPresetName,
        error,
        clearError,
    };
}
