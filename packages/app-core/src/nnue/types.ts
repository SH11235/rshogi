/**
 * NNUE ファイル管理の型定義
 */

/**
 * NNUE フォーマット情報
 */
export interface NnueFormat {
    /** アーキテクチャ文字列（例: "HalfKA1024", "LayerStacks"） */
    architecture: string;

    /** L1 次元（例: 256, 512, 1024, 1536） */
    l1Dimension: number;

    /** L2 次元（例: 8, 32） */
    l2Dimension: number;

    /** L3 次元（例: 32, 96） */
    l3Dimension: number;

    /** 活性化関数（"CReLU" or "SCReLU"） */
    activation: string;

    /** バージョンヘッダ（16進数文字列） */
    versionHeader: string;
}

/**
 * NNUE メタデータ
 */
export interface NnueMeta {
    /**
     * アプリ内識別子（UUID v4）
     * SHA-256 とは別。ID は不変、ハッシュは内容に依存
     */
    id: string;

    /** 表示名（ユーザーが付けた名前 or ファイル名） */
    displayName: string;

    /** 元のファイル名 */
    originalFileName: string;

    /** ファイルサイズ（バイト） */
    size: number;

    /**
     * ファイル内容の SHA-256 ハッシュ
     * - 重複検出
     * - 破損検知
     * - プリセット更新判定
     */
    contentHashSha256: string;

    /** ソース種別 */
    source: "user-uploaded" | "preset" | "url";

    /** ソース URL（source が 'url' or 'preset' の場合） */
    sourceUrl?: string;

    /**
     * プリセットキー（source が 'preset' の場合）
     * manifest.json の presetKey と対応
     * 例: 'suisho5', 'custom-stable', 'custom-latest'
     */
    presetKey?: string;

    /** 登録日時（Unix timestamp） */
    createdAt: number;

    /** 最終使用日時（Unix timestamp） */
    lastUsedAt?: number;

    /** NNUE フォーマット情報（自動検出） */
    format?: NnueFormat;

    // --- 検証情報 ---

    /** エンジンでロード成功したか */
    verified: boolean;

    /** 検証成功した日時（Unix timestamp） */
    verifiedAt?: number;

    /** 検証時のエンジンバージョン（将来の互換性チェック用） */
    verifiedWithEngineVersion?: string;

    // --- ライセンス情報（プリセット配布に必要） ---

    /** ライセンス名（例: 'MIT', 'GPL-3.0', 'Proprietary'） */
    license?: string;

    /** ライセンス全文の URL */
    licenseUrl?: string;

    /** 帰属表示（必要な場合） */
    attribution?: string;

    /** リリース日（プリセットの場合） */
    releasedAt?: string;
}

/**
 * エンジン設定プロファイル
 */
export interface EngineProfile {
    /** 一意識別子（UUID v4） */
    id: string;

    /** プロファイル名 */
    name: string;

    /** エンジン種別 */
    type: "builtin" | "external-usi";

    /** 内蔵エンジン設定 */
    builtin?: {
        /** 使用する NNUE の ID（NnueMeta.id） */
        nnueId?: string;

        /** スレッド数 */
        threads: number;

        /** 置換表サイズ (MB) */
        hashMb: number;

        /** MultiPV */
        multiPv: number;

        /** スキルレベル（0-20, undefined = 最強） */
        skillLevel?: number;
    };

    /** 外部 USI エンジン設定（Desktop 限定） */
    externalUsi?: {
        /** 実行ファイルパス */
        executablePath: string;

        /** 作業ディレクトリ */
        workingDirectory?: string;

        /** USI オプション */
        options: Record<string, string | number | boolean>;

        /** NNUE ファイルパス（EvalFile オプション用） */
        nnuePath?: string;
    };

    /** 作成日時（Unix timestamp） */
    createdAt: number;

    /** 最終使用日時（Unix timestamp） */
    lastUsedAt?: number;

    /** デフォルトフラグ */
    isDefault: boolean;
}

/**
 * NNUE ダウンロード進捗
 */
export interface NnueDownloadProgress {
    /** ダウンロード対象の presetKey（プリセットの場合）または UUID */
    targetKey: string;
    /** ダウンロード済みバイト数 */
    loaded: number;
    /** 総バイト数 */
    total: number;
    /** 現在のフェーズ */
    phase: "downloading" | "saving" | "validating";
}

/**
 * プリセット更新情報
 */
export interface PresetUpdate {
    /** プリセットキー */
    presetKey: string;
    /** ローカルに存在するバージョン一覧（複数バージョンがあり得る） */
    currentVersions: string[];
    /** 新しいバージョン */
    newVersion: string;
    /** 新しいバージョンの SHA-256 */
    newSha256: string;
}

/**
 * プリセット設定（manifest.json の各エントリ）
 */
export interface PresetConfig {
    /** プリセット識別キー */
    presetKey: string;
    /** 表示名 */
    displayName: string;
    /** 説明 */
    description: string;
    /** ダウンロード URL */
    url: string;
    /** ファイルサイズ（バイト） */
    size: number;
    /** SHA-256 ハッシュ */
    sha256: string;
    /** ライセンス名 */
    license: string;
    /** ライセンス URL */
    licenseUrl?: string;
    /** リリース日 */
    releasedAt: string;
    /** フォーマット情報 */
    format?: Partial<NnueFormat>;
}

/**
 * プリセット manifest
 */
export interface PresetManifest {
    /** manifest バージョン */
    version: number;
    /** 更新日時（ISO 8601） */
    updatedAt: string;
    /** プリセット一覧 */
    presets: PresetConfig[];
}

/**
 * プリセットの状態
 */
export type PresetStatus = "latest" | "update-available" | "not-downloaded";

/**
 * NNUE 選択状態
 *
 * プリセット指定とカスタムNNUE指定を区別するための型。
 * - presetKey が設定されている場合: プリセットを使用（遅延ダウンロード対応）
 * - presetKey が null で nnueId が設定されている場合: カスタムNNUEを使用
 * - 両方 null の場合: NNUEなし（駒得評価）
 */
export interface NnueSelection {
    /** プリセットキー（優先、これが設定されていればプリセット使用） */
    presetKey: string | null;
    /** カスタムNNUEのID（presetKeyがnullの場合に使用） */
    nnueId: string | null;
}

/** デフォルトプリセットキー */
export const DEFAULT_PRESET_KEY = "ramu";

/** デフォルトのNNUE選択（最初のプリセット） */
export const DEFAULT_NNUE_SELECTION: NnueSelection = {
    presetKey: DEFAULT_PRESET_KEY,
    nnueId: null,
};

/** NNUEなし（駒得評価）の選択 */
export const NONE_NNUE_SELECTION: NnueSelection = {
    presetKey: null,
    nnueId: null,
};
