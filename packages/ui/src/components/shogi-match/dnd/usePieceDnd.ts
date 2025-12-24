/**
 * 将棋盤 編集モード DnD 本体 hook
 *
 * ref + rAF でパフォーマンス最適化
 * PointerEvents を使用し、タッチ・マウス両対応
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { dropTargetEquals, getDropTarget } from "./hitDetection";
import type {
    DndConfig,
    DndState,
    DragOrigin,
    DragPayload,
    DragRuntime,
    DropResult,
    DropTarget,
} from "./types";
import { DEFAULT_DND_CONFIG } from "./types";

/** ゴーストのサイズ (px) - DragGhost.tsx の h-12 w-12 と同期 */
const GHOST_SIZE = 48;
/** ゴースト位置オフセット（中心に配置するため） */
const GHOST_OFFSET = GHOST_SIZE / 2;

interface UsePieceDndOptions {
    /** ドロップ時のコールバック */
    onDrop?: (result: DropResult) => void;
    /** キャンセル時のコールバック */
    onCancel?: (origin: DragOrigin, reason: string) => void;
    /** 設定 */
    config?: Partial<DndConfig>;
    /** 無効化 */
    disabled?: boolean;
}

interface PieceDndController {
    /** React state（低頻度更新） */
    state: DndState;
    /** ゴースト要素の ref */
    ghostRef: React.RefObject<HTMLElement | null>;
    /** ドラッグ開始（PointerEvent から呼び出す） */
    startDrag: (
        origin: DragOrigin,
        payload: DragPayload,
        e: PointerEvent | React.PointerEvent,
    ) => void;
    /** ドラッグキャンセル */
    cancelDrag: (reason: string) => void;
    /** クリーンアップ（useEffect 用） */
    cleanup: () => void;
}

function createInitialRuntime(): DragRuntime {
    return {
        active: false,
        pointerId: null,
        pointerType: null,
        captureTarget: null,
        startClient: { x: 0, y: 0 },
        lastClient: { x: 0, y: 0 },
        longPressTimer: null,
        raf: null,
        origin: null,
        payload: null,
        hover: null,
    };
}

export function usePieceDnd(options: UsePieceDndOptions): PieceDndController {
    const { onDrop, onCancel, config: configOverrides, disabled = false } = options;

    const config: DndConfig = { ...DEFAULT_DND_CONFIG, ...configOverrides };

    // React state（低頻度更新）
    const [state, setState] = useState<DndState>({
        isDragging: false,
        payload: null,
        hoverTarget: null,
        mode: null,
    });

    // Mutable runtime（高頻度更新）
    const runtimeRef = useRef<DragRuntime>(createInitialRuntime());
    const ghostRef = useRef<HTMLElement | null>(null);

    // タッチ操作時のイベントリスナー参照（メモリリーク防止）
    const touchListenersRef = useRef<{
        checkSlop: ((e: PointerEvent) => void) | null;
        cancelOnUp: ((e: PointerEvent) => void) | null;
    }>({ checkSlop: null, cancelOnUp: null });

    // クリーンアップ関数
    const cleanup = useCallback(() => {
        const rt = runtimeRef.current;

        // タイマー解除
        if (rt.longPressTimer !== null) {
            clearTimeout(rt.longPressTimer);
        }

        // rAF 解除
        if (rt.raf !== null) {
            cancelAnimationFrame(rt.raf);
        }

        // タッチリスナーのクリーンアップ（メモリリーク防止）
        const tl = touchListenersRef.current;
        if (tl.checkSlop) {
            document.removeEventListener("pointermove", tl.checkSlop);
            tl.checkSlop = null;
        }
        if (tl.cancelOnUp) {
            document.removeEventListener("pointerup", tl.cancelOnUp);
            document.removeEventListener("pointercancel", tl.cancelOnUp);
            tl.cancelOnUp = null;
        }

        // ゴースト非表示
        if (ghostRef.current) {
            ghostRef.current.style.display = "none";
        }

        // Pointer capture 解放（既に失われていても例外にしない）
        if (rt.pointerId !== null && rt.captureTarget) {
            try {
                rt.captureTarget.releasePointerCapture(rt.pointerId);
            } catch {
                // 既に解放済み
            }
        }

        // Runtime リセット
        runtimeRef.current = createInitialRuntime();

        // React state 更新
        setState({
            isDragging: false,
            payload: null,
            hoverTarget: null,
            mode: null,
        });
    }, []);

    // ゴースト位置更新（rAF 経由）
    const updateGhostPosition = useCallback((x: number, y: number) => {
        if (ghostRef.current) {
            ghostRef.current.style.transform = `translate3d(${x}px, ${y}px, 0)`;
        }
    }, []);

    // ホバーターゲット更新（変化時のみ state 更新）
    const updateHoverTarget = useCallback((target: DropTarget | null) => {
        const rt = runtimeRef.current;
        if (!dropTargetEquals(rt.hover, target)) {
            rt.hover = target;

            // モード計算
            let mode: DndState["mode"] = null;
            if (target) {
                mode = target.type === "delete" ? "delete" : "valid";
            }

            setState((prev) => ({
                ...prev,
                hoverTarget: target,
                mode,
            }));
        }
    }, []);

    // PointerMove ハンドラ
    const handlePointerMove = useCallback(
        (e: PointerEvent) => {
            const rt = runtimeRef.current;
            if (!rt.active || rt.pointerId !== e.pointerId) return;

            rt.lastClient = { x: e.clientX, y: e.clientY };

            // rAF でゴースト更新
            if (rt.raf === null) {
                rt.raf = requestAnimationFrame(() => {
                    rt.raf = null;
                    const { x, y } = rt.lastClient;

                    // ゴースト位置更新（中心に配置）
                    updateGhostPosition(x - GHOST_OFFSET, y - GHOST_OFFSET);

                    // ヒットテスト（DOM の data 属性から直接判定）
                    const target = getDropTarget(x, y, config.outsideAreaBehavior);
                    updateHoverTarget(target);
                });
            }
        },
        [config.outsideAreaBehavior, updateGhostPosition, updateHoverTarget],
    );

    // PointerUp ハンドラ（ドロップ）
    const handlePointerUp = useCallback(
        (e: PointerEvent) => {
            const rt = runtimeRef.current;
            if (!rt.active || rt.pointerId !== e.pointerId) return;

            const origin = rt.origin;
            const payload = rt.payload;
            const target = rt.hover;

            cleanup();

            if (origin && payload && target && onDrop) {
                onDrop({ origin, payload, target });
            }
        },
        [cleanup, onDrop],
    );

    // PointerCancel / LostPointerCapture ハンドラ
    const handlePointerCancel = useCallback(
        (e: PointerEvent) => {
            const rt = runtimeRef.current;
            if (rt.pointerId !== e.pointerId) return;

            const origin = rt.origin;
            cleanup();

            if (origin && onCancel) {
                onCancel(origin, "pointercancel");
            }
        },
        [cleanup, onCancel],
    );

    // VisibilityChange ハンドラ
    const handleVisibilityChange = useCallback(() => {
        if (document.visibilityState === "hidden") {
            const rt = runtimeRef.current;
            if (rt.active) {
                const origin = rt.origin;
                cleanup();
                if (origin && onCancel) {
                    onCancel(origin, "visibilitychange");
                }
            }
        }
    }, [cleanup, onCancel]);

    // Resize ハンドラ（ドラッグ中はキャンセル）
    const handleResize = useCallback(() => {
        const rt = runtimeRef.current;
        if (rt.active) {
            const origin = rt.origin;
            cleanup();
            if (origin && onCancel) {
                onCancel(origin, "resize");
            }
        }
    }, [cleanup, onCancel]);

    // イベントリスナー登録
    useEffect(() => {
        if (disabled) return;

        document.addEventListener("pointermove", handlePointerMove);
        document.addEventListener("pointerup", handlePointerUp);
        document.addEventListener("pointercancel", handlePointerCancel);
        document.addEventListener("lostpointercapture", handlePointerCancel);
        document.addEventListener("visibilitychange", handleVisibilityChange);
        window.addEventListener("resize", handleResize);

        return () => {
            document.removeEventListener("pointermove", handlePointerMove);
            document.removeEventListener("pointerup", handlePointerUp);
            document.removeEventListener("pointercancel", handlePointerCancel);
            document.removeEventListener("lostpointercapture", handlePointerCancel);
            document.removeEventListener("visibilitychange", handleVisibilityChange);
            window.removeEventListener("resize", handleResize);
        };
    }, [
        disabled,
        handlePointerMove,
        handlePointerUp,
        handlePointerCancel,
        handleVisibilityChange,
        handleResize,
    ]);

    // アンマウント時のクリーンアップ
    useEffect(() => {
        return () => {
            cleanup();
        };
    }, [cleanup]);

    // ドラッグ開始
    const startDrag = useCallback(
        (origin: DragOrigin, payload: DragPayload, e: PointerEvent | React.PointerEvent) => {
            if (disabled) return;

            const rt = runtimeRef.current;

            // 既にドラッグ中なら無視
            if (rt.active) return;

            const pointerId = e.pointerId;
            const pointerType = e.pointerType as "mouse" | "touch" | "pen";
            const clientX = e.clientX;
            const clientY = e.clientY;

            const activateDrag = () => {
                rt.active = true;
                rt.pointerId = pointerId;
                rt.pointerType = pointerType;
                rt.origin = origin;
                rt.payload = payload;
                rt.startClient = { x: clientX, y: clientY };
                rt.lastClient = { x: clientX, y: clientY };

                // Pointer capture
                const captureEl = e.target as Element;
                try {
                    captureEl.setPointerCapture(pointerId);
                    rt.captureTarget = captureEl;
                } catch {
                    // 失敗しても続行
                    rt.captureTarget = null;
                }

                // ゴースト表示
                if (ghostRef.current) {
                    ghostRef.current.style.display = "block";
                    updateGhostPosition(clientX - GHOST_OFFSET, clientY - GHOST_OFFSET);
                }

                // 初期ヒットテスト（DOM の data 属性から直接判定）
                const dropTarget = getDropTarget(clientX, clientY, config.outsideAreaBehavior);
                rt.hover = dropTarget;

                // React state 更新
                setState({
                    isDragging: true,
                    payload,
                    hoverTarget: rt.hover,
                    mode: rt.hover?.type === "delete" ? "delete" : "valid",
                });
            };

            if (pointerType === "touch") {
                // タッチ: ロングプレス + スロップ判定
                rt.startClient = { x: clientX, y: clientY };

                // リスナークリーンアップ用のヘルパー
                const cleanupTouchListeners = () => {
                    const tl = touchListenersRef.current;
                    if (tl.checkSlop) {
                        document.removeEventListener("pointermove", tl.checkSlop);
                        tl.checkSlop = null;
                    }
                    if (tl.cancelOnUp) {
                        document.removeEventListener("pointerup", tl.cancelOnUp);
                        document.removeEventListener("pointercancel", tl.cancelOnUp);
                        tl.cancelOnUp = null;
                    }
                };

                const checkSlop = (moveEvent: PointerEvent) => {
                    if (moveEvent.pointerId !== pointerId) return;
                    const dx = moveEvent.clientX - rt.startClient.x;
                    const dy = moveEvent.clientY - rt.startClient.y;
                    const distance = Math.sqrt(dx * dx + dy * dy);

                    if (distance > config.slopPx) {
                        // スロップ超過 → キャンセル（スクロールに譲る）
                        if (rt.longPressTimer !== null) {
                            clearTimeout(rt.longPressTimer);
                            rt.longPressTimer = null;
                        }
                        cleanupTouchListeners();
                    }
                };

                const cancelOnUp = (upEvent: PointerEvent) => {
                    if (upEvent.pointerId !== pointerId) return;
                    if (rt.longPressTimer !== null) {
                        clearTimeout(rt.longPressTimer);
                        rt.longPressTimer = null;
                    }
                    cleanupTouchListeners();
                };

                // ref に保存してクリーンアップ時にアクセス可能にする
                touchListenersRef.current.checkSlop = checkSlop;
                touchListenersRef.current.cancelOnUp = cancelOnUp;

                document.addEventListener("pointermove", checkSlop);
                document.addEventListener("pointerup", cancelOnUp);
                document.addEventListener("pointercancel", cancelOnUp);

                rt.longPressTimer = setTimeout(() => {
                    rt.longPressTimer = null;
                    cleanupTouchListeners();
                    activateDrag();
                }, config.longPressMs);
            } else {
                // マウス/ペン: スロップ判定後に開始（クリックとドラッグを区別）
                rt.startClient = { x: clientX, y: clientY };
                rt.pointerId = pointerId;
                rt.pointerType = pointerType;
                rt.origin = origin;
                rt.payload = payload;

                const checkMouseSlop = (moveEvent: PointerEvent) => {
                    if (moveEvent.pointerId !== pointerId) return;
                    const dx = moveEvent.clientX - rt.startClient.x;
                    const dy = moveEvent.clientY - rt.startClient.y;
                    const distance = Math.sqrt(dx * dx + dy * dy);

                    if (distance > config.slopPx) {
                        // スロップ超過 → DnD開始
                        document.removeEventListener("pointermove", checkMouseSlop);
                        document.removeEventListener("pointerup", cancelMouseOnUp);
                        activateDrag();
                    }
                };

                const cancelMouseOnUp = (upEvent: PointerEvent) => {
                    if (upEvent.pointerId !== pointerId) return;
                    // クリック扱い（DnD開始せずに終了）
                    document.removeEventListener("pointermove", checkMouseSlop);
                    document.removeEventListener("pointerup", cancelMouseOnUp);
                    // runtimeをリセット
                    rt.pointerId = null;
                    rt.pointerType = null;
                    rt.origin = null;
                    rt.payload = null;
                };

                document.addEventListener("pointermove", checkMouseSlop);
                document.addEventListener("pointerup", cancelMouseOnUp);
            }
        },
        [
            disabled,
            config.longPressMs,
            config.slopPx,
            config.outsideAreaBehavior,
            updateGhostPosition,
        ],
    );

    // キャンセル
    const cancelDrag = useCallback(
        (reason: string) => {
            const rt = runtimeRef.current;
            if (!rt.active) return;

            const origin = rt.origin;
            cleanup();

            if (origin && onCancel) {
                onCancel(origin, reason);
            }
        },
        [cleanup, onCancel],
    );

    return {
        state,
        ghostRef,
        startDrag,
        cancelDrag,
        cleanup,
    };
}
