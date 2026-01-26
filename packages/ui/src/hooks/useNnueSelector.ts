import { useCallback, useState } from "react";

export interface UseNnueSelectorReturn {
    /** モーダルが開いているか */
    isOpen: boolean;
    /** 選択中の NNUE ID（null = NNUEなし（Material評価）） */
    selectedNnueId: string | null;
    /** モーダルを開く */
    open: () => void;
    /** モーダルを閉じる（選択をキャンセル） */
    close: () => void;
    /** NNUE を選択（確定前） */
    select: (id: string | null) => void;
    /** 選択を確定してモーダルを閉じる */
    confirm: () => void;
    /** エンジン再起動警告を表示すべきか */
    showRestartWarning: boolean;
}

export interface UseNnueSelectorOptions {
    /** 現在エンジンで使用中の NNUE ID */
    currentNnueId: string | null;
    /** 選択確定時のコールバック */
    onConfirm?: (nnueId: string | null) => void;
    /** エンジンが初期化済みか（再起動警告の表示判定用） */
    isEngineInitialized?: boolean;
}

/**
 * NNUE 選択モーダルの状態を管理するフック
 */
export function useNnueSelector({
    currentNnueId,
    onConfirm,
    isEngineInitialized = false,
}: UseNnueSelectorOptions): UseNnueSelectorReturn {
    const [isOpen, setIsOpen] = useState(false);
    const [selectedNnueId, setSelectedNnueId] = useState<string | null>(currentNnueId);

    const open = useCallback(() => {
        setSelectedNnueId(currentNnueId);
        setIsOpen(true);
    }, [currentNnueId]);

    const close = useCallback(() => {
        setIsOpen(false);
    }, []);

    const select = useCallback((id: string | null) => {
        setSelectedNnueId(id);
    }, []);

    const confirm = useCallback(() => {
        onConfirm?.(selectedNnueId);
        setIsOpen(false);
    }, [selectedNnueId, onConfirm]);

    // 選択が変更された場合のみ警告を表示
    const showRestartWarning = isEngineInitialized && selectedNnueId !== currentNnueId;

    return {
        isOpen,
        selectedNnueId,
        open,
        close,
        select,
        confirm,
        showRestartWarning,
    };
}
