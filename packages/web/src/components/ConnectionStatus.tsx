import { cn } from "@/lib/utils";
import type { ConnectionProgress, ConnectionQuality } from "@/services/webrtc";
import type { ConnectionStatus } from "@/types/online";
import { AlertCircle, Loader2, Wifi, WifiOff } from "lucide-react";

interface ConnectionStatusProps {
    status: ConnectionStatus;
    quality?: ConnectionQuality;
    progress?: ConnectionProgress;
    onReconnect?: () => void;
}

export function ConnectionStatusComponent({
    status,
    quality,
    progress,
    onReconnect,
}: ConnectionStatusProps) {
    const getStatusColor = () => {
        switch (status.connectionState) {
            case "connected":
                return "text-green-600";
            case "connecting":
                return "text-yellow-600";
            case "disconnected":
            case "failed":
                return "text-red-600";
            default:
                return "text-gray-600";
        }
    };

    const getStatusText = () => {
        if (progress && progress !== "idle" && progress !== "connected") {
            return getProgressText(progress);
        }

        switch (status.connectionState) {
            case "connected":
                return "接続済み";
            case "connecting":
                return "接続中...";
            case "disconnected":
                return "切断されました";
            case "failed":
                return "接続失敗";
            case "new":
                return "未接続";
            default:
                return status.connectionState;
        }
    };

    const getProgressText = (progress: ConnectionProgress): string => {
        switch (progress) {
            case "creating_offer":
                return "部屋を作成中...";
            case "waiting_answer":
                return "相手の返答待ち";
            case "establishing":
                return "接続を確立中...";
            case "ice_gathering":
                return "ネットワーク情報を収集中...";
            case "ice_checking":
                return "接続ルートを確認中...";
            default:
                return progress;
        }
    };

    const getQualityIndicator = () => {
        if (!quality || !status.isConnected) return null;

        const { latency } = quality;
        let color = "text-green-600";
        let label = "良好";

        if (latency > 150) {
            color = "text-red-600";
            label = "不安定";
        } else if (latency > 80) {
            color = "text-yellow-600";
            label = "普通";
        }

        return (
            <span className={cn("text-xs", color)}>
                {label} ({latency}ms)
            </span>
        );
    };

    const isLoading =
        progress && progress !== "idle" && progress !== "connected" && progress !== "failed";

    return (
        <div className="flex items-center gap-2 text-sm">
            {isLoading ? (
                <Loader2 className="h-4 w-4 animate-spin text-blue-600" />
            ) : status.isConnected ? (
                <Wifi className={cn("h-4 w-4", getStatusColor())} />
            ) : (
                <WifiOff className={cn("h-4 w-4", getStatusColor())} />
            )}

            <div className="flex flex-col">
                <div className="flex items-center gap-2">
                    <span className={getStatusColor()}>{getStatusText()}</span>
                    {status.isHost && <span className="text-xs text-gray-500">(ホスト)</span>}
                    {getQualityIndicator()}
                </div>

                {status.connectionState === "failed" && (
                    <span className="text-xs text-red-600 flex items-center gap-1">
                        <AlertCircle className="h-3 w-3" />
                        接続に問題が発生しました
                    </span>
                )}
            </div>

            {(status.connectionState === "disconnected" || status.connectionState === "failed") &&
                onReconnect && (
                    <button
                        onClick={onReconnect}
                        className="ml-2 text-xs text-blue-600 hover:text-blue-800 underline"
                        type="button"
                    >
                        再接続
                    </button>
                )}
        </div>
    );
}
