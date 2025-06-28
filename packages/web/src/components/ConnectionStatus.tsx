import type { ConnectionStatus } from "@/types/online";
import { Wifi, WifiOff } from "lucide-react";

interface ConnectionStatusProps {
    status: ConnectionStatus;
    onReconnect?: () => void;
}

export function ConnectionStatusComponent({ status, onReconnect }: ConnectionStatusProps) {
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

    return (
        <div className="flex items-center gap-2 text-sm">
            {status.isConnected ? (
                <Wifi className={`h-4 w-4 ${getStatusColor()}`} />
            ) : (
                <WifiOff className={`h-4 w-4 ${getStatusColor()}`} />
            )}
            <span className={getStatusColor()}>{getStatusText()}</span>
            {status.isHost && <span className="text-xs text-gray-500">(ホスト)</span>}
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
