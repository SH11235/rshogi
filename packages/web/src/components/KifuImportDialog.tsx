import { cn } from "@/lib/utils";
import { useCallback, useRef, useState } from "react";
import { parseKifMoves, validateKifFormat, validateSfenFormat } from "shogi-core";
import type { Move } from "shogi-core";
import { Alert, AlertDescription } from "./ui/alert";
import { Button } from "./ui/button";
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogFooter,
    DialogHeader,
    DialogTitle,
} from "./ui/dialog";
import { Label } from "./ui/label";
import { RadioGroup, RadioGroupItem } from "./ui/radio-group";
import { Textarea } from "./ui/textarea";

export type ImportFormat = "kif" | "sfen";

interface KifuImportDialogProps {
    open: boolean;
    onOpenChange: (open: boolean) => void;
    onImport: (moves: Move[], format: ImportFormat, content?: string) => void;
}

export function KifuImportDialog({ open, onOpenChange, onImport }: KifuImportDialogProps) {
    const [content, setContent] = useState("");
    const [format, setFormat] = useState<ImportFormat>("kif");
    const [error, setError] = useState<string | null>(null);
    const [isDragging, setIsDragging] = useState(false);
    const fileInputRef = useRef<HTMLInputElement>(null);

    const handleFileSelect = useCallback((event: React.ChangeEvent<HTMLInputElement>) => {
        const file = event.target.files?.[0];
        if (!file) return;

        const reader = new FileReader();
        reader.onload = (e) => {
            const text = e.target?.result as string;
            setContent(text);
            setError(null);

            // ファイル拡張子から形式を推測
            if (file.name.toLowerCase().endsWith(".sfen")) {
                setFormat("sfen");
            } else if (
                file.name.toLowerCase().endsWith(".kif") ||
                file.name.toLowerCase().endsWith(".kifu")
            ) {
                setFormat("kif");
            }
        };
        reader.onerror = () => {
            setError("ファイルの読み込みに失敗しました");
        };
        reader.readAsText(file, "utf-8");

        // ファイル選択をリセット
        event.target.value = "";
    }, []);

    const handleDragEnter = useCallback((e: React.DragEvent) => {
        e.preventDefault();
        e.stopPropagation();
        setIsDragging(true);
    }, []);

    const handleDragLeave = useCallback((e: React.DragEvent) => {
        e.preventDefault();
        e.stopPropagation();
        setIsDragging(false);
    }, []);

    const handleDragOver = useCallback((e: React.DragEvent) => {
        e.preventDefault();
        e.stopPropagation();
    }, []);

    const handleDrop = useCallback((e: React.DragEvent) => {
        e.preventDefault();
        e.stopPropagation();
        setIsDragging(false);

        const files = Array.from(e.dataTransfer.files);
        const textFile = files.find(
            (file) =>
                file.type === "text/plain" ||
                file.name.endsWith(".kif") ||
                file.name.endsWith(".kifu") ||
                file.name.endsWith(".sfen"),
        );

        if (textFile) {
            const reader = new FileReader();
            reader.onload = (event) => {
                const text = event.target?.result as string;
                setContent(text);
                setError(null);

                // ファイル拡張子から形式を推測
                if (textFile.name.toLowerCase().endsWith(".sfen")) {
                    setFormat("sfen");
                } else {
                    setFormat("kif");
                }
            };
            reader.onerror = () => {
                setError("ファイルの読み込みに失敗しました");
            };
            reader.readAsText(textFile, "utf-8");
        } else {
            setError("テキストファイルをドロップしてください");
        }
    }, []);

    const handleImport = useCallback(() => {
        if (!content.trim()) {
            setError("棋譜を入力してください");
            return;
        }

        try {
            if (format === "kif") {
                const validation = validateKifFormat(content);
                if (!validation.valid) {
                    setError(validation.error || "無効なKIF形式です");
                    return;
                }

                const parseResult = parseKifMoves(content);
                if (parseResult.moves.length === 0) {
                    setError("棋譜から手順を読み取れませんでした");
                    return;
                }

                onImport(parseResult.moves, format, content);
                onOpenChange(false);
                setContent("");
                setError(null);
            } else if (format === "sfen") {
                const validation = validateSfenFormat(content);
                if (!validation.valid) {
                    setError(validation.error || "無効なSFEN形式です");
                    return;
                }

                // SFEN形式の場合は局面のみで手順がないため、空の配列とコンテンツを渡す
                onImport([], format, content);
                onOpenChange(false);
                setContent("");
                setError(null);
            }
        } catch (err) {
            setError(err instanceof Error ? err.message : "解析中にエラーが発生しました");
        }
    }, [content, format, onImport, onOpenChange]);

    const handleClose = useCallback(() => {
        onOpenChange(false);
        setContent("");
        setError(null);
        setFormat("kif");
    }, [onOpenChange]);

    return (
        <Dialog open={open} onOpenChange={handleClose}>
            <DialogContent className="sm:max-w-[600px]">
                <DialogHeader>
                    <DialogTitle>棋譜をインポート</DialogTitle>
                    <DialogDescription>
                        KIF形式またはSFEN形式の棋譜を貼り付けるか、ファイルを選択してください
                    </DialogDescription>
                </DialogHeader>

                <div className="space-y-4">
                    <div>
                        <Label htmlFor="format">形式</Label>
                        <RadioGroup
                            id="format"
                            value={format}
                            onValueChange={(value) => setFormat(value as ImportFormat)}
                            className="flex gap-4 mt-2"
                        >
                            <div className="flex items-center space-x-2">
                                <RadioGroupItem value="kif" id="kif" />
                                <Label htmlFor="kif" className="font-normal cursor-pointer">
                                    KIF形式（棋譜）
                                </Label>
                            </div>
                            <div className="flex items-center space-x-2">
                                <RadioGroupItem value="sfen" id="sfen" />
                                <Label htmlFor="sfen" className="font-normal cursor-pointer">
                                    SFEN形式（局面）
                                </Label>
                            </div>
                        </RadioGroup>
                    </div>

                    <div>
                        <Label htmlFor="content">棋譜データ</Label>
                        <div
                            className={cn(
                                "relative rounded-md border-2 border-dashed p-2 transition-colors",
                                isDragging
                                    ? "border-primary bg-primary/10"
                                    : "border-muted-foreground/25",
                            )}
                            onDragEnter={handleDragEnter}
                            onDragLeave={handleDragLeave}
                            onDragOver={handleDragOver}
                            onDrop={handleDrop}
                        >
                            <Textarea
                                id="content"
                                value={content}
                                onChange={(e) => {
                                    setContent(e.target.value);
                                    setError(null);
                                }}
                                placeholder={
                                    format === "kif"
                                        ? "KIF形式の棋譜を貼り付けてください...\n\n例:\n# KIF形式棋譜ファイル\n手数----指手---------消費時間--\n   1 ７六歩(77)   ( 0:00/00:00:00)"
                                        : "SFEN形式の局面を貼り付けてください...\n\n例:\nlnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"
                                }
                                className="min-h-[200px] font-mono text-sm resize-none"
                            />
                            {isDragging && (
                                <div className="absolute inset-0 flex items-center justify-center bg-background/80 rounded-md">
                                    <p className="text-lg font-medium">ファイルをドロップ</p>
                                </div>
                            )}
                        </div>
                        <p className="text-sm text-muted-foreground mt-2">
                            またはファイルをドラッグ&ドロップ
                        </p>
                    </div>

                    <div className="flex items-center gap-2">
                        <input
                            ref={fileInputRef}
                            type="file"
                            onChange={handleFileSelect}
                            accept=".kif,.kifu,.sfen,.txt"
                            className="hidden"
                        />
                        <Button
                            type="button"
                            variant="outline"
                            onClick={() => fileInputRef.current?.click()}
                        >
                            ファイルを選択
                        </Button>
                        <span className="text-sm text-muted-foreground">
                            対応形式: .kif, .kifu, .sfen, .txt
                        </span>
                    </div>

                    {error && (
                        <Alert variant="destructive">
                            <AlertDescription>{error}</AlertDescription>
                        </Alert>
                    )}
                </div>

                <DialogFooter>
                    <Button variant="outline" onClick={handleClose}>
                        キャンセル
                    </Button>
                    <Button onClick={handleImport}>インポート</Button>
                </DialogFooter>
            </DialogContent>
        </Dialog>
    );
}
