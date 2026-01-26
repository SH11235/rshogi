import { type ReactElement, useCallback, useMemo, useState } from "react";
import { useNnueStorage } from "../../hooks/useNnueStorage";
import { usePresetManager } from "../../hooks/usePresetManager";
import { Button } from "../button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "../dialog";
import { NnueErrorAlert } from "./NnueErrorAlert";
import { NnueImportArea } from "./NnueImportArea";
import { NnueListItem } from "./NnueListItem";
import { NnueProgressOverlay } from "./NnueProgressOverlay";
import { PresetListItem } from "./PresetListItem";

interface NnueManagerDialogProps {
    /** モーダルが開いているか */
    open: boolean;
    /** モーダルを閉じる時のコールバック */
    onOpenChange: (open: boolean) => void;
    /** プリセット manifest.json の URL（指定時のみプリセット機能が有効） */
    manifestUrl?: string;
    /** Desktop 用: ファイル選択ダイアログを開いてパスを取得するコールバック */
    onRequestFilePath?: () => Promise<string | null>;
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
            }}
        >
            <div
                style={{
                    padding: "12px",
                    borderRadius: "6px",
                    backgroundColor: "hsl(var(--muted, 0 0% 96%))",
                    fontSize: "13px",
                }}
            >
                <div
                    style={{
                        fontWeight: 600,
                        marginBottom: "8px",
                        display: "flex",
                        justifyContent: "space-between",
                        alignItems: "center",
                    }}
                >
                    <span>ストレージについて</span>
                    <span style={{ fontWeight: 400 }}>
                        使用量: {(totalSize / (1024 * 1024)).toFixed(1)} MB
                    </span>
                </div>
                <div
                    style={{
                        display: "flex",
                        flexDirection: "column",
                        gap: "6px",
                    }}
                >
                    <p style={{ margin: 0 }}>NNUE ファイルはブラウザのストレージに保存されます。</p>
                    <p style={{ margin: 0 }}>
                        ブラウザの設定やストレージ不足により、自動削除される可能性があります。
                    </p>
                </div>
            </div>
        </div>
    );
}

/**
 * NNUE ファイル管理モーダル
 *
 * NNUE 一覧表示、インポート、削除を提供する。
 * NNUE の選択機能は含まない（対局設定や分析設定で行う）。
 */
export function NnueManagerDialog({
    open,
    onOpenChange,
    manifestUrl,
    onRequestFilePath,
}: NnueManagerDialogProps): ReactElement {
    const {
        nnueList,
        isLoading: isStorageLoading,
        error: storageError,
        importFromFile,
        importFromPath,
        deleteNnue,
        updateDisplayName,
        clearError: clearStorageError,
        refreshList,
        capabilities,
    } = useNnueStorage();

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
        onDownloadComplete: () => {
            // ダウンロード完了時にストレージを更新
            void refreshList();
        },
    });

    const [isImporting, setIsImporting] = useState(false);
    const [deletingId, setDeletingId] = useState<string | null>(null);

    const handleFileSelect = useCallback(
        async (file: File) => {
            setIsImporting(true);
            try {
                await importFromFile(file);
            } catch {
                // エラーは useNnueStorage で管理される
            } finally {
                setIsImporting(false);
            }
        },
        [importFromFile],
    );

    // Desktop 用: ファイルダイアログでパスを取得してインポート
    const handleRequestFilePath = useCallback(async () => {
        if (!onRequestFilePath) return;
        setIsImporting(true);
        try {
            const filePath = await onRequestFilePath();
            if (filePath) {
                await importFromPath(filePath);
            }
        } catch {
            // エラーは useNnueStorage で管理される
        } finally {
            setIsImporting(false);
        }
    }, [onRequestFilePath, importFromPath]);

    const handleDelete = useCallback(
        async (id: string) => {
            setDeletingId(id);
            try {
                await deleteNnue(id);
            } catch {
                // エラーは useNnueStorage で管理される
            } finally {
                setDeletingId(null);
            }
        },
        [deleteNnue],
    );

    const handleDisplayNameChange = useCallback(
        async (id: string, newName: string) => {
            await updateDisplayName(id, newName);
        },
        [updateDisplayName],
    );

    const handleClose = useCallback(() => {
        onOpenChange(false);
    }, [onOpenChange]);

    const handleClearError = useCallback(() => {
        clearStorageError();
        clearPresetError();
    }, [clearStorageError, clearPresetError]);

    const error = storageError ?? presetError;
    const isOperationInProgress = isImporting || deletingId !== null || downloadingKey !== null;

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
                    <DialogTitle>評価関数（NNUE) ファイル管理</DialogTitle>
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

                    {/* NNUE 一覧（選択なし、削除のみ） */}
                    {nnueList.length > 0 ? (
                        <div style={{ display: "flex", flexDirection: "column", gap: "8px" }}>
                            <div
                                style={{
                                    fontSize: "12px",
                                    fontWeight: 500,
                                    color: "hsl(var(--muted-foreground, 0 0% 45%))",
                                    marginBottom: "4px",
                                }}
                            >
                                インポート済み ({nnueList.length})
                            </div>
                            {nnueList.map((meta) => (
                                <NnueListItem
                                    key={meta.id}
                                    meta={meta}
                                    selectable={false}
                                    onDelete={() => handleDelete(meta.id)}
                                    isDeleting={deletingId === meta.id}
                                    disabled={isOperationInProgress}
                                    onDisplayNameChange={(newName) =>
                                        handleDisplayNameChange(meta.id, newName)
                                    }
                                />
                            ))}
                        </div>
                    ) : (
                        <div
                            style={{
                                fontSize: "13px",
                                color: "hsl(var(--muted-foreground, 0 0% 45%))",
                                textAlign: "center",
                                padding: "16px",
                            }}
                        >
                            インポートされた NNUE ファイルはありません
                        </div>
                    )}

                    {/* プリセット一覧（manifestUrl が設定されている場合のみ） */}
                    {/* 最新版ダウンロード済みのものは除外（インポート済みに表示されるため） */}
                    {isPresetConfigured &&
                        presets.filter((p) => p.status !== "latest").length > 0 && (
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
                                {presets
                                    .filter((p) => p.status !== "latest")
                                    .map((preset) => (
                                        <PresetListItem
                                            key={preset.config.presetKey}
                                            preset={preset}
                                            selectable={false}
                                            onDownload={downloadPreset}
                                            isDownloading={
                                                downloadingKey === preset.config.presetKey
                                            }
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

                <DialogFooter style={{ justifyContent: "center" }}>
                    <Button variant="secondary" onClick={handleClose}>
                        閉じる
                    </Button>
                </DialogFooter>
            </DialogContent>
        </Dialog>
    );
}
