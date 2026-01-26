import {
    createPresetManager,
    NNUE_HEADER_SIZE,
    type NnueDownloadProgress,
    NnueError,
    type NnueMeta,
    type PresetManager,
    type PresetWithStatus,
} from "@shogi/app-core";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useNnueContextOptional } from "../providers/NnueContext";

interface UsePresetManagerReturn {
    /** プリセット一覧（状態付き） */
    presets: PresetWithStatus[];
    /** manifest 読み込み中 */
    isLoading: boolean;
    /** ダウンロード中のプリセットキー */
    downloadingKey: string | null;
    /** ダウンロード進捗 */
    downloadProgress: NnueDownloadProgress | null;
    /** エラー */
    error: NnueError | null;
    /** manifest を再取得 */
    refresh: () => Promise<void>;
    /** プリセットをダウンロード（成功時は NnueMeta を返す） */
    download: (presetKey: string) => Promise<NnueMeta | undefined>;
    /** エラーをクリア */
    clearError: () => void;
    /** manifest URL が設定されているか */
    isConfigured: boolean;
}

interface UsePresetManagerOptions {
    /** manifest.json の URL */
    manifestUrl?: string;
    /** プリセット一覧を自動取得するか */
    autoFetch?: boolean;
    /** ダウンロード完了時のコールバック（ダウンロードした NnueMeta を受け取る） */
    onDownloadComplete?: (meta: NnueMeta) => void;
}

/**
 * プリセット NNUE 管理フック
 *
 * manifest.json からプリセット一覧を取得し、ダウンロード状態を管理する。
 * NnueProvider の外で使用すると、空の状態を返す。
 */
export function usePresetManager(options: UsePresetManagerOptions = {}): UsePresetManagerReturn {
    const { manifestUrl, autoFetch = true, onDownloadComplete } = options;
    const context = useNnueContextOptional();
    const storage = context?.storage ?? null;
    const validateNnueHeader = context?.validateNnueHeader ?? null;

    const [presets, setPresets] = useState<PresetWithStatus[]>([]);
    const [isLoading, setIsLoading] = useState(false);
    const [downloadingKey, setDownloadingKey] = useState<string | null>(null);
    const [downloadProgress, setDownloadProgress] = useState<NnueDownloadProgress | null>(null);
    const [error, setError] = useState<NnueError | null>(null);

    const isConfigured = Boolean(manifestUrl && storage);

    // PresetManager インスタンスを作成
    const manager = useMemo<PresetManager | null>(() => {
        if (!manifestUrl || !storage) return null;
        return createPresetManager({
            manifestUrl,
            storage,
            onProgress: setDownloadProgress,
        });
    }, [manifestUrl, storage]);

    // プリセット一覧を取得
    const refresh = useCallback(async () => {
        if (!manager) return;
        setIsLoading(true);
        setError(null);
        try {
            const statuses = await manager.getPresetStatuses();
            setPresets(statuses);
        } catch (e) {
            const err =
                e instanceof NnueError
                    ? e
                    : new NnueError(
                          "NNUE_DOWNLOAD_FAILED",
                          "プリセット一覧の取得に失敗しました",
                          e,
                      );
            setError(err);
        } finally {
            setIsLoading(false);
        }
    }, [manager]);

    // 初回自動取得
    useEffect(() => {
        if (autoFetch && manager) {
            void refresh();
        }
    }, [autoFetch, manager, refresh]);

    const validatePresetMeta = useCallback(
        async (meta: NnueMeta) => {
            if (!storage || !validateNnueHeader) return;
            if (!storage.capabilities.supportsLoad || !storage.load) return;

            try {
                const data = await storage.load(meta.id);
                const header = data.subarray(0, Math.min(NNUE_HEADER_SIZE, data.byteLength));
                const result = await validateNnueHeader(header);
                if (result.isCompatible && result.format) {
                    await storage.updateMeta(meta.id, { format: result.format });
                }
            } catch {
                // 検証失敗時はフォーマット情報を更新しない
            }
        },
        [storage, validateNnueHeader],
    );

    // プリセットをダウンロード
    const download = useCallback(
        async (presetKey: string): Promise<NnueMeta | undefined> => {
            if (!manager) {
                setError(
                    new NnueError(
                        "NNUE_STORAGE_FAILED",
                        "プリセットマネージャーが初期化されていません",
                    ),
                );
                return undefined;
            }

            // 既にダウンロード中の場合は無視
            if (downloadingKey) {
                setError(
                    new NnueError("NNUE_DOWNLOAD_IN_PROGRESS", "別のダウンロードが進行中です"),
                );
                return undefined;
            }

            setDownloadingKey(presetKey);
            setDownloadProgress(null);
            setError(null);

            try {
                // 重複チェック - 既存のメタを取得して返す
                const isDuplicate = await manager.isDuplicate(presetKey);
                if (isDuplicate) {
                    // 既に同じハッシュのファイルがある場合は既存のメタを取得
                    const manifest = await manager.getManifest();
                    const preset = manifest.presets.find((p) => p.presetKey === presetKey);
                    if (preset && storage) {
                        const existing = await storage.listByContentHash(preset.sha256);
                        if (existing.length > 0) {
                            await validatePresetMeta(existing[0]);
                            await refresh();
                            onDownloadComplete?.(existing[0]);
                            return existing[0];
                        }
                    }
                    await refresh();
                    return undefined;
                }

                const meta = await manager.download(presetKey);
                await validatePresetMeta(meta);
                await refresh();
                onDownloadComplete?.(meta);
                return meta;
            } catch (e) {
                const err =
                    e instanceof NnueError
                        ? e
                        : new NnueError("NNUE_DOWNLOAD_FAILED", "ダウンロードに失敗しました", e);
                setError(err);
                return undefined;
            } finally {
                setDownloadingKey(null);
                setDownloadProgress(null);
            }
        },
        [manager, storage, downloadingKey, refresh, onDownloadComplete, validatePresetMeta],
    );

    const clearError = useCallback(() => {
        setError(null);
    }, []);

    return {
        presets,
        isLoading,
        downloadingKey,
        downloadProgress,
        error,
        refresh,
        download,
        clearError,
        isConfigured,
    };
}

/**
 * プリセット設定からダウンロード済みの NnueMeta を取得するヘルパー
 */
export function getDownloadedMeta(preset: PresetWithStatus): {
    meta: import("@shogi/app-core").NnueMeta | null;
    isLatest: boolean;
} {
    if (preset.localMetas.length === 0) {
        return { meta: null, isLatest: false };
    }

    // 最新のハッシュと一致するものを優先
    const latestMeta = preset.localMetas.find((m) => m.contentHashSha256 === preset.config.sha256);
    if (latestMeta) {
        return { meta: latestMeta, isLatest: true };
    }

    // なければ最新の作成日時のものを返す
    const sorted = [...preset.localMetas].sort((a, b) => b.createdAt - a.createdAt);
    return { meta: sorted[0], isLatest: false };
}
