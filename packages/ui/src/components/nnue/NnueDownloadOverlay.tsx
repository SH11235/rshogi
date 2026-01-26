import type { NnueDownloadProgress } from "@shogi/app-core";
import type { ReactElement } from "react";
import { Progress } from "../progress";
import { Spinner } from "../spinner";

interface NnueDownloadOverlayProps {
    /** 表示するかどうか */
    visible: boolean;
    /** ダウンロード進捗 */
    progress: NnueDownloadProgress | null;
    /** プリセット名 */
    presetName?: string | null;
}

function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function getPhaseLabel(phase: NnueDownloadProgress["phase"]): string {
    switch (phase) {
        case "downloading":
            return "ダウンロード中";
        case "validating":
            return "検証中";
        case "saving":
            return "保存中";
        default:
            return "処理中";
    }
}

/**
 * NNUE ダウンロード中オーバーレイ
 *
 * プリセット NNUE の遅延ダウンロード中に表示する。
 */
export function NnueDownloadOverlay({
    visible,
    progress,
    presetName,
}: NnueDownloadOverlayProps): ReactElement | null {
    if (!visible) return null;

    const progressPercent =
        progress && progress.total > 0 ? Math.round((progress.loaded / progress.total) * 100) : 0;

    return (
        <div
            style={{
                position: "fixed",
                inset: 0,
                backgroundColor: "rgba(0, 0, 0, 0.5)",
                display: "flex",
                flexDirection: "column",
                alignItems: "center",
                justifyContent: "center",
                gap: "16px",
                zIndex: 100,
            }}
        >
            <div
                style={{
                    backgroundColor: "hsl(var(--card, 0 0% 100%))",
                    borderRadius: "12px",
                    padding: "32px 48px",
                    display: "flex",
                    flexDirection: "column",
                    alignItems: "center",
                    gap: "16px",
                    boxShadow: "0 8px 30px rgba(0, 0, 0, 0.2)",
                    minWidth: "300px",
                }}
            >
                <Spinner size="xl" label="評価関数をダウンロード中" />
                <div
                    style={{
                        color: "hsl(var(--foreground, 0 0% 10%))",
                        fontWeight: 500,
                        fontSize: "16px",
                        textAlign: "center",
                    }}
                >
                    評価関数をダウンロード中...
                </div>
                {presetName && (
                    <div
                        style={{
                            color: "hsl(var(--muted-foreground, 0 0% 45%))",
                            fontSize: "14px",
                        }}
                    >
                        {presetName}
                    </div>
                )}

                {/* プログレスバー */}
                {progress && (
                    <div
                        style={{
                            width: "100%",
                            display: "flex",
                            flexDirection: "column",
                            gap: "8px",
                        }}
                    >
                        <Progress value={progressPercent} style={{ height: "8px" }} />
                        <div
                            style={{
                                display: "flex",
                                justifyContent: "space-between",
                                fontSize: "12px",
                                color: "hsl(var(--muted-foreground, 0 0% 45%))",
                            }}
                        >
                            <span>{getPhaseLabel(progress.phase)}</span>
                            <span>
                                {formatSize(progress.loaded)} / {formatSize(progress.total)} (
                                {progressPercent}%)
                            </span>
                        </div>
                    </div>
                )}

                <div
                    style={{
                        color: "hsl(var(--muted-foreground, 0 0% 45%))",
                        fontSize: "13px",
                    }}
                >
                    しばらくお待ちください
                </div>
            </div>
        </div>
    );
}
