import type { ReactElement } from "react";
import {
    AlertDialog,
    AlertDialogAction,
    AlertDialogCancel,
    AlertDialogContent,
    AlertDialogDescription,
    AlertDialogFooter,
    AlertDialogHeader,
    AlertDialogTitle,
    AlertDialogTrigger,
} from "../../alert-dialog";
import { Button } from "../../button";

interface PlayingModeControlsProps {
    /** 停止ボタンのクリックハンドラ */
    onStop: () => void;
    /** 投了ボタンのクリックハンドラ */
    onResign?: () => void;
    /** 待ったボタンのクリックハンドラ */
    onUndo?: () => void;
    /** 待った可能かどうか */
    canUndo?: boolean;
}

/**
 * 対局中のコントロールボタン
 * 停止・投了・待った
 */
export function PlayingModeControls({
    onStop,
    onResign,
    onUndo,
    canUndo = false,
}: PlayingModeControlsProps): ReactElement {
    return (
        <>
            <Button type="button" onClick={onStop} variant="destructive">
                停止
            </Button>
            {onResign && (
                <AlertDialog>
                    <AlertDialogTrigger asChild>
                        <Button type="button" variant="outline">
                            投了
                        </Button>
                    </AlertDialogTrigger>
                    <AlertDialogContent>
                        <AlertDialogHeader>
                            <AlertDialogTitle>投了しますか？</AlertDialogTitle>
                            <AlertDialogDescription>対局を終了します。</AlertDialogDescription>
                        </AlertDialogHeader>
                        <AlertDialogFooter>
                            <AlertDialogCancel>キャンセル</AlertDialogCancel>
                            <AlertDialogAction onClick={onResign}>投了する</AlertDialogAction>
                        </AlertDialogFooter>
                    </AlertDialogContent>
                </AlertDialog>
            )}
            {onUndo && (
                <Button type="button" onClick={onUndo} variant="outline" disabled={!canUndo}>
                    待った
                </Button>
            )}
        </>
    );
}

interface PausedModeControlsProps {
    /** 対局再開ボタンのクリックハンドラ */
    onResume: () => void;
    /** 局面編集ボタンのクリックハンドラ */
    onEnterEditMode?: () => void;
    /** 投了ボタンのクリックハンドラ */
    onResign?: () => void;
}

/**
 * 一時停止中のコントロールボタン
 * 対局再開・局面編集・投了
 */
export function PausedModeControls({
    onResume,
    onEnterEditMode,
    onResign,
}: PausedModeControlsProps): ReactElement {
    return (
        <>
            <Button type="button" onClick={onResume}>
                対局再開
            </Button>
            {onEnterEditMode && (
                <AlertDialog>
                    <AlertDialogTrigger asChild>
                        <Button type="button" variant="outline">
                            局面編集
                        </Button>
                    </AlertDialogTrigger>
                    <AlertDialogContent>
                        <AlertDialogHeader>
                            <AlertDialogTitle>局面編集モードに入りますか？</AlertDialogTitle>
                            <AlertDialogDescription>
                                現在の対局を中断して編集モードに移行します。
                            </AlertDialogDescription>
                        </AlertDialogHeader>
                        <AlertDialogFooter className="sm:justify-center">
                            <AlertDialogCancel>キャンセル</AlertDialogCancel>
                            <AlertDialogAction onClick={onEnterEditMode}>
                                編集モードへ
                            </AlertDialogAction>
                        </AlertDialogFooter>
                    </AlertDialogContent>
                </AlertDialog>
            )}
            {onResign && (
                <AlertDialog>
                    <AlertDialogTrigger asChild>
                        <Button type="button" variant="outline">
                            投了
                        </Button>
                    </AlertDialogTrigger>
                    <AlertDialogContent>
                        <AlertDialogHeader>
                            <AlertDialogTitle>投了しますか？</AlertDialogTitle>
                            <AlertDialogDescription>対局を終了します。</AlertDialogDescription>
                        </AlertDialogHeader>
                        <AlertDialogFooter>
                            <AlertDialogCancel>キャンセル</AlertDialogCancel>
                            <AlertDialogAction onClick={onResign}>投了する</AlertDialogAction>
                        </AlertDialogFooter>
                    </AlertDialogContent>
                </AlertDialog>
            )}
        </>
    );
}
