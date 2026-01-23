/**
 * プリセット NNUE 管理
 *
 * manifest.json からのプリセット一覧取得、ダウンロード、検証を行う
 */

import { NNUE_MAX_SIZE_BYTES, NNUE_PROGRESS_THROTTLE_MS } from "./constants";
import { NnueError } from "./errors";
import type { NnueStorage } from "./storage";
import type {
    NnueDownloadProgress,
    NnueMeta,
    PresetConfig,
    PresetManifest,
    PresetStatus,
} from "./types";

/**
 * プリセットダウンロード進捗ハンドラー
 */
export type PresetProgressHandler = (progress: NnueDownloadProgress) => void;

/**
 * プリセットマネージャーのオプション
 */
export interface PresetManagerOptions {
    /** manifest.json の URL */
    manifestUrl: string;
    /** NNUE ストレージ */
    storage: NnueStorage;
    /** 進捗ハンドラー */
    onProgress?: PresetProgressHandler;
}

/**
 * プリセットとその状態
 */
export interface PresetWithStatus {
    config: PresetConfig;
    status: PresetStatus;
    /** ローカルに存在する場合の NnueMeta（複数バージョンがあり得る） */
    localMetas: NnueMeta[];
}

/**
 * manifest.json を取得
 */
export async function fetchPresetManifest(manifestUrl: string): Promise<PresetManifest> {
    try {
        const response = await fetch(manifestUrl);
        if (!response.ok) {
            throw new NnueError(
                "NNUE_DOWNLOAD_FAILED",
                `manifest.json の取得に失敗しました: ${response.status}`,
            );
        }
        const manifest = (await response.json()) as PresetManifest;
        return manifest;
    } catch (error) {
        if (error instanceof NnueError) throw error;
        throw new NnueError(
            "NNUE_DOWNLOAD_FAILED",
            "manifest.json の取得に失敗しました。ネットワークを確認してください。",
            error,
        );
    }
}

/**
 * SHA-256 ハッシュを計算
 */
async function computeSha256(data: ArrayBuffer): Promise<string> {
    const hashBuffer = await crypto.subtle.digest("SHA-256", data);
    const hashArray = Array.from(new Uint8Array(hashBuffer));
    return hashArray.map((b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * プリセットをダウンロード
 */
export async function downloadPreset(
    preset: PresetConfig,
    storage: NnueStorage,
    onProgress?: PresetProgressHandler,
): Promise<NnueMeta> {
    // サイズチェック（ダウンロード前）
    if (preset.size > NNUE_MAX_SIZE_BYTES) {
        throw new NnueError(
            "NNUE_SIZE_EXCEEDED",
            `ファイルサイズが上限（${Math.round(NNUE_MAX_SIZE_BYTES / 1024 / 1024)}MB）を超えています`,
        );
    }

    // 進捗通知: ダウンロード開始
    onProgress?.({
        targetKey: preset.presetKey,
        loaded: 0,
        total: preset.size,
        phase: "downloading",
    });

    // ダウンロード
    let response: Response;
    try {
        response = await fetch(preset.url);
        if (!response.ok) {
            throw new NnueError(
                "NNUE_DOWNLOAD_FAILED",
                `ダウンロードに失敗しました: ${response.status}`,
            );
        }
    } catch (error) {
        if (error instanceof NnueError) throw error;
        throw new NnueError(
            "NNUE_DOWNLOAD_FAILED",
            "ダウンロードに失敗しました。ネットワークを確認してください。",
            error,
        );
    }

    // Content-Length がある場合はサイズ検証（ただし Content-Encoding がない場合のみ）
    // CDN が gzip/transfer-encoding を使う場合、Content-Length は圧縮後のサイズになる可能性がある
    const contentLength = response.headers.get("Content-Length");
    const contentEncoding = response.headers.get("Content-Encoding");
    if (contentLength && !contentEncoding && Number(contentLength) !== preset.size) {
        throw new NnueError(
            "NNUE_SIZE_MISMATCH",
            `ファイルサイズが一致しません（期待: ${preset.size}, 実際: ${contentLength}）`,
        );
    }

    // 進捗付きダウンロード
    const reader = response.body?.getReader();
    if (!reader) {
        throw new NnueError("NNUE_DOWNLOAD_FAILED", "レスポンスボディを読み取れません");
    }

    const chunks: Uint8Array[] = [];
    let loaded = 0;
    let lastProgressTime = 0;

    try {
        while (true) {
            const { done, value } = await reader.read();
            if (done) break;

            chunks.push(value);
            loaded += value.byteLength;

            // スロットリングされた進捗通知
            const now = Date.now();
            if (now - lastProgressTime >= NNUE_PROGRESS_THROTTLE_MS) {
                onProgress?.({
                    targetKey: preset.presetKey,
                    loaded,
                    total: preset.size,
                    phase: "downloading",
                });
                lastProgressTime = now;
            }
        }
    } finally {
        reader.releaseLock();
    }

    // ダウンロード完了後のサイズ検証
    if (loaded !== preset.size) {
        throw new NnueError(
            "NNUE_SIZE_MISMATCH",
            `ファイルサイズが一致しません（期待: ${preset.size}, 実際: ${loaded}）`,
        );
    }

    // 進捗通知: 検証中
    onProgress?.({
        targetKey: preset.presetKey,
        loaded,
        total: preset.size,
        phase: "validating",
    });

    // チャンクを結合
    const data = new Uint8Array(loaded);
    let offset = 0;
    for (const chunk of chunks) {
        data.set(chunk, offset);
        offset += chunk.byteLength;
    }

    // SHA-256 検証
    const hash = await computeSha256(data.buffer);
    if (hash !== preset.sha256) {
        throw new NnueError("NNUE_HASH_MISMATCH", "ファイル検証に失敗しました（ハッシュ不一致）");
    }

    // 進捗通知: 保存中
    onProgress?.({
        targetKey: preset.presetKey,
        loaded,
        total: preset.size,
        phase: "saving",
    });

    // ID を生成
    const id = crypto.randomUUID();

    // メタデータを作成
    const meta: NnueMeta = {
        id,
        displayName: preset.displayName,
        originalFileName: `${preset.presetKey}.nnue`,
        size: data.byteLength,
        contentHashSha256: hash,
        source: "preset",
        sourceUrl: preset.url,
        presetKey: preset.presetKey,
        createdAt: Date.now(),
        verified: false,
        license: preset.license,
        licenseUrl: preset.licenseUrl,
        releasedAt: preset.releasedAt,
        format: preset.format
            ? {
                  architecture: preset.format.architecture ?? "unknown",
                  l1Dimension: preset.format.l1Dimension ?? 0,
                  l2Dimension: preset.format.l2Dimension ?? 0,
                  l3Dimension: preset.format.l3Dimension ?? 0,
                  activation: preset.format.activation ?? "unknown",
                  versionHeader: preset.format.versionHeader ?? "",
              }
            : undefined,
    };

    // 保存
    await storage.save(id, data, meta);

    return meta;
}

/**
 * プリセットの状態を取得
 */
export async function getPresetStatus(
    preset: PresetConfig,
    storage: NnueStorage,
): Promise<{ status: PresetStatus; localMetas: NnueMeta[] }> {
    // 同じ presetKey のローカル NNUE を検索
    const localMetas = await storage.listByPresetKey(preset.presetKey);

    if (localMetas.length === 0) {
        return { status: "not-downloaded", localMetas: [] };
    }

    // SHA-256 が一致するものがあれば最新
    const hasLatest = localMetas.some((m) => m.contentHashSha256 === preset.sha256);
    if (hasLatest) {
        return { status: "latest", localMetas };
    }

    // presetKey は存在するが SHA-256 が異なる = 更新あり
    return { status: "update-available", localMetas };
}

/**
 * 全プリセットの状態を取得
 */
export async function getAllPresetStatuses(
    manifest: PresetManifest,
    storage: NnueStorage,
): Promise<PresetWithStatus[]> {
    const results: PresetWithStatus[] = [];

    for (const config of manifest.presets) {
        const { status, localMetas } = await getPresetStatus(config, storage);
        results.push({ config, status, localMetas });
    }

    return results;
}

/**
 * プリセットマネージャーを作成
 *
 * manifest 取得、ダウンロード、状態管理をまとめたオブジェクトを返す
 */
export function createPresetManager(options: PresetManagerOptions) {
    const { manifestUrl, storage, onProgress } = options;

    let cachedManifest: PresetManifest | null = null;

    return {
        /**
         * manifest を取得（キャッシュあり）
         */
        async getManifest(forceRefresh = false): Promise<PresetManifest> {
            if (!cachedManifest || forceRefresh) {
                cachedManifest = await fetchPresetManifest(manifestUrl);
            }
            return cachedManifest;
        },

        /**
         * 全プリセットの状態を取得
         */
        async getPresetStatuses(): Promise<PresetWithStatus[]> {
            const manifest = await this.getManifest();
            return getAllPresetStatuses(manifest, storage);
        },

        /**
         * プリセットをダウンロード
         */
        async download(presetKey: string): Promise<NnueMeta> {
            const manifest = await this.getManifest();
            const preset = manifest.presets.find((p) => p.presetKey === presetKey);
            if (!preset) {
                throw new NnueError("NNUE_NOT_FOUND", `プリセット "${presetKey}" が見つかりません`);
            }
            return downloadPreset(preset, storage, onProgress);
        },

        /**
         * 重複チェック（同じハッシュが既に存在するか）
         */
        async isDuplicate(presetKey: string): Promise<boolean> {
            const manifest = await this.getManifest();
            const preset = manifest.presets.find((p) => p.presetKey === presetKey);
            if (!preset) return false;

            const existing = await storage.listByContentHash(preset.sha256);
            return existing.length > 0;
        },

        /**
         * キャッシュをクリア
         */
        clearCache(): void {
            cachedManifest = null;
        },
    };
}

export type PresetManager = ReturnType<typeof createPresetManager>;
