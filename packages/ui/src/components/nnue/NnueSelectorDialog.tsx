import { type ReactElement, useCallback, useEffect, useMemo, useState } from "react";
import { useNnueSelector } from "../../hooks/useNnueSelector";
import { useNnueStorage } from "../../hooks/useNnueStorage";
import { usePresetManager } from "../../hooks/usePresetManager";
import { Button } from "../button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "../dialog";
import { NnueErrorAlert } from "./NnueErrorAlert";
import { NnueImportArea } from "./NnueImportArea";
import { NnueList } from "./NnueList";
import { NnueProgressOverlay } from "./NnueProgressOverlay";
import { PresetListItem } from "./PresetListItem";

export interface NnueSelectorDialogProps {
    /** モーダルが開いているか */
    open: boolean;
    /** モーダルを閉じる時のコールバック */
    onOpenChange: (open: boolean) => void;
    /** 現在エンジンで使用中の NNUE ID（null = デフォルト） */
    currentNnueId: string | null;
    /** NNUE 選択確定時のコールバック */
    onNnueChange?: (nnueId: string | null) => void;
    /** エンジンが初期化済みか（再起動警告表示用） */
    isEngineInitialized?: boolean;
    /** プリセット manifest.json の URL（指定時のみプリセット機能が有効） */
    manifestUrl?: string;
    /** Desktop 用: ファイル選択ダイアログを開いてパスを取得するコールバック */
    onRequestFilePath?: () => Promise<string | null>;
    /** 解析中かどうか（解析中は NNUE 変更不可） */
    isAnalyzing?: boolean;
}

/**
 * NNUE ストレージ使用量と注意事項を表示するコンポーネント
 */
function NnueStorageInfo({ totalSize }: { totalSize: number }): ReactElement {
    return (
        <div
            style={{
                fontSize: "12px",
                color: "hsl(var(--muted-foreground, 0 0% 45%))",
                textAlign: "right",
            }}
        >
            <div>NNUE 使用量: {(totalSize / (1024 * 1024)).toFixed(1)} MB</div>
            <div style={{ fontSize: "11px", marginTop: "4px" }}>
                ※ ブラウザのストレージに保存。容量不足時に削除される可能性があります。
            </div>
        </div>
    );
}

/**
 * NNUE 選択モーダル
 *
 * NNUE 一覧表示、インポート、削除、選択を一つのモーダルで提供する。
 */
export function NnueSelectorDialog({
    open,
    onOpenChange,
    currentNnueId,
    onNnueChange,
    isEngineInitialized = false,
    manifestUrl,
    onRequestFilePath,
    isAnalyzing = false,
}: NnueSelectorDialogProps): ReactElement {
    const {
        nnueList,
        isLoading: isStorageLoading,
        error: storageError,
        importFromFile,
        importFromPath,
        deleteNnue,
        clearError: clearStorageError,
        refreshList,
        capabilities,
    } = useNnueStorage();

    const { selectedNnueId, select, showRestartWarning } = useNnueSelector({
        currentNnueId,
        isEngineInitialized,
    });

    // ダイアログを開いた時に選択状態を currentNnueId にリセット
    useEffect(() => {
        if (open) {
            select(currentNnueId);
        }
    }, [open, currentNnueId, select]);

    const {
        presets,
        isLoading: isPresetsLoading,
        downloadingKey,
        downloadProgress,
        error: presetError,
        download: downloadPreset,
        clearError: clearPresetError,
        isConfigured: isPresetConfigured,
    } = usePresetManager({
        manifestUrl,
        autoFetch: open && Boolean(manifestUrl),
        onDownloadComplete: (meta) => {
            // ダウンロード完了時にストレージを更新して自動選択
            void refreshList();
            select(meta.id);
        },
    });

    const [isImporting, setIsImporting] = useState(false);
    const [deletingId, setDeletingId] = useState<string | null>(null);

    const handleFileSelect = useCallback(
        async (file: File) => {
            setIsImporting(true);
            try {
                const meta = await importFromFile(file);
                // インポート後は自動的にその NNUE を選択
                select(meta.id);
            } catch {
                // エラーは useNnueStorage で管理される
            } finally {
                setIsImporting(false);
            }
        },
        [importFromFile, select],
    );

    // Desktop 用: ファイルダイアログでパスを取得してインポート
    const handleRequestFilePath = useCallback(async () => {
        if (!onRequestFilePath) return;
        setIsImporting(true);
        try {
            const filePath = await onRequestFilePath();
            if (filePath) {
                const meta = await importFromPath(filePath);
                select(meta.id);
            }
        } catch {
            // エラーは useNnueStorage で管理される
        } finally {
            setIsImporting(false);
        }
    }, [onRequestFilePath, importFromPath, select]);

    const handleDelete = useCallback(
        async (id: string) => {
            setDeletingId(id);
            try {
                await deleteNnue(id);
                // 削除した NNUE が選択中だった場合はデフォルトに戻す
                if (selectedNnueId === id) {
                    select(null);
                }
            } catch {
                // エラーは useNnueStorage で管理される
            } finally {
                setDeletingId(null);
            }
        },
        [deleteNnue, selectedNnueId, select],
    );

    const handleConfirm = useCallback(() => {
        // 選択を確定して閉じる（実際のロードは useEngineManager 側で行う）
        onNnueChange?.(selectedNnueId);
        onOpenChange(false);
    }, [selectedNnueId, onNnueChange, onOpenChange]);

    const handleCancel = useCallback(() => {
        onOpenChange(false);
    }, [onOpenChange]);

    const handleClearError = useCallback(() => {
        clearStorageError();
        clearPresetError();
    }, [clearStorageError, clearPresetError]);

    const error = storageError ?? presetError;
    const isOperationInProgress = isImporting || deletingId !== null || downloadingKey !== null;
    // 解析中は NNUE 変更を許可しない（暗黙のキャンセルを防ぐため）
    const isConfirmDisabled = isOperationInProgress || isAnalyzing;

    // NNUE ファイルの合計サイズを計算
    const totalNnueSize = useMemo(
        () => nnueList.reduce((sum, meta) => sum + meta.size, 0),
        [nnueList],
    );

    return (
        <Dialog open={open} onOpenChange={onOpenChange}>
            <DialogContent
                style={{
                    width: "min(520px, calc(100% - 24px))",
                    maxHeight: "80vh",
                    display: "flex",
                    flexDirection: "column",
                }}
            >
                <DialogHeader>
                    <DialogTitle>NNUE ファイル選択</DialogTitle>
                </DialogHeader>

                <div
                    style={{
                        flex: 1,
                        overflow: "auto",
                        display: "flex",
                        flexDirection: "column",
                        gap: "16px",
                        position: "relative",
                        minHeight: "200px",
                    }}
                >
                    {/* エラー表示 */}
                    <NnueErrorAlert error={error} onClose={handleClearError} />

                    {/* NNUE 一覧 */}
                    <NnueList
                        nnueList={nnueList}
                        selectedId={selectedNnueId}
                        onSelect={select}
                        onDelete={handleDelete}
                        deletingId={deletingId}
                        disabled={isOperationInProgress}
                    />

                    {/* プリセット一覧（manifestUrl が設定されている場合のみ） */}
                    {isPresetConfigured && presets.length > 0 && (
                        <div style={{ display: "flex", flexDirection: "column", gap: "8px" }}>
                            <div
                                style={{
                                    fontSize: "12px",
                                    fontWeight: 500,
                                    color: "hsl(var(--muted-foreground, 0 0% 45%))",
                                    marginTop: "8px",
                                    marginBottom: "4px",
                                }}
                            >
                                ダウンロード可能なプリセット
                            </div>
                            {presets.map((preset) => (
                                <PresetListItem
                                    key={preset.config.presetKey}
                                    preset={preset}
                                    isSelected={preset.localMetas.some(
                                        (m) => m.id === selectedNnueId,
                                    )}
                                    onSelect={select}
                                    onDownload={downloadPreset}
                                    isDownloading={downloadingKey === preset.config.presetKey}
                                    downloadProgress={
                                        downloadingKey === preset.config.presetKey
                                            ? downloadProgress
                                            : null
                                    }
                                    disabled={isOperationInProgress}
                                />
                            ))}
                        </div>
                    )}

                    {/* プリセット読み込み中 */}
                    {isPresetConfigured && isPresetsLoading && (
                        <div
                            style={{
                                fontSize: "13px",
                                color: "hsl(var(--muted-foreground, 0 0% 45%))",
                                textAlign: "center",
                                padding: "16px",
                            }}
                        >
                            プリセット一覧を読み込み中...
                        </div>
                    )}

                    {/* インポートエリア */}
                    {capabilities && (
                        <NnueImportArea
                            capabilities={capabilities}
                            onFileSelect={handleFileSelect}
                            onRequestFilePath={handleRequestFilePath}
                            isImporting={isImporting}
                            disabled={isOperationInProgress}
                        />
                    )}

                    {/* NNUE 使用量 */}
                    <NnueStorageInfo totalSize={totalNnueSize} />

                    {/* 進捗オーバーレイ */}
                    <NnueProgressOverlay
                        visible={isStorageLoading && !isImporting}
                        message="読み込み中..."
                    />
                </div>

                {/* 解析中警告 */}
                {isAnalyzing && (
                    <div
                        style={{
                            display: "flex",
                            alignItems: "center",
                            gap: "8px",
                            padding: "8px 12px",
                            borderRadius: "6px",
                            backgroundColor: "hsl(var(--muted, 0 0% 96%))",
                            color: "hsl(var(--muted-foreground, 0 0% 45%))",
                            fontSize: "13px",
                        }}
                    >
                        <svg
                            width="16"
                            height="16"
                            viewBox="0 0 16 16"
                            fill="none"
                            aria-hidden="true"
                        >
                            <circle cx="8" cy="8" r="6.5" stroke="currentColor" strokeWidth="1.5" />
                            <path
                                d="M8 5v3.5"
                                stroke="currentColor"
                                strokeWidth="1.5"
                                strokeLinecap="round"
                            />
                            <circle cx="8" cy="11" r="0.5" fill="currentColor" />
                        </svg>
                        解析中は変更できません
                    </div>
                )}

                {/* 再起動警告 */}
                {!isAnalyzing && showRestartWarning && (
                    <div
                        style={{
                            display: "flex",
                            alignItems: "center",
                            gap: "8px",
                            padding: "8px 12px",
                            borderRadius: "6px",
                            backgroundColor: "hsl(var(--warning, 38 92% 50%) / 0.1)",
                            color: "hsl(var(--warning, 38 92% 50%))",
                            fontSize: "13px",
                        }}
                    >
                        <svg
                            width="16"
                            height="16"
                            viewBox="0 0 16 16"
                            fill="none"
                            aria-hidden="true"
                        >
                            <path
                                d="M8 1L15 14H1L8 1Z"
                                stroke="currentColor"
                                strokeWidth="1.5"
                                strokeLinejoin="round"
                            />
                            <path
                                d="M8 6v4"
                                stroke="currentColor"
                                strokeWidth="1.5"
                                strokeLinecap="round"
                            />
                            <circle cx="8" cy="12" r="0.5" fill="currentColor" />
                        </svg>
                        変更するとエンジンが再起動されます
                    </div>
                )}

                <DialogFooter>
                    <Button variant="outline" onClick={handleCancel}>
                        キャンセル
                    </Button>
                    <Button onClick={handleConfirm} disabled={isConfirmDisabled}>
                        {showRestartWarning ? "適用して再起動" : "適用"}
                    </Button>
                </DialogFooter>
            </DialogContent>
        </Dialog>
    );
}
