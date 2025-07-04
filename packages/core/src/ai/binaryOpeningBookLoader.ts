import { decode } from "@msgpack/msgpack";
import * as pako from "pako";
import type { Move } from "../domain/model/move";
import type { PieceType } from "../domain/model/piece";
import type { Column, Row } from "../domain/model/square";
import {
    type MoveBinary,
    MoveFlags,
    type OpeningEntryBinary,
    PieceTypeBinary,
    indexToCoordinate,
} from "./binaryOpeningTypes";
import type { OpeningEntry, OpeningMove } from "./openingBook";
import { OpeningBookLoader } from "./openingBookLoader";

/**
 * バイナリ形式の定跡データベースローダー
 * MessagePack形式のファイルを効率的に読み込み
 */
export class BinaryOpeningBookLoader extends OpeningBookLoader {
    /**
     * バイナリ形式のファイルを処理
     */
    protected async processResponse(response: Response, fileName: string): Promise<void> {
        try {
            // データの取得と解析
            const arrayBuffer = await response.arrayBuffer();
            const uint8Array = new Uint8Array(arrayBuffer);

            // 最初の数バイトでデータ形式を判定
            let data: { format?: string; entries: OpeningEntryBinary[] };
            if (uint8Array.length >= 2 && uint8Array[0] === 0x1f && uint8Array[1] === 0x8b) {
                // gzip圧縮されている
                const decompressed = pako.ungzip(uint8Array);

                // MessagePackかJSONかを判定
                if (this.isMsgPack(decompressed)) {
                    data = decode(decompressed) as {
                        format?: string;
                        entries: OpeningEntryBinary[];
                    };
                } else {
                    const jsonString = new TextDecoder().decode(decompressed);
                    data = JSON.parse(jsonString);
                }
            } else if (this.isMsgPack(uint8Array)) {
                // 非圧縮のMessagePack
                data = decode(uint8Array) as { format?: string; entries: OpeningEntryBinary[] };
            } else {
                // 通常のJSON処理にフォールバック
                return super.processResponse(response, fileName);
            }

            // バイナリ形式の場合は変換が必要
            if (data.format === "msgpack") {
                const convertedEntries = this.convertBinaryEntries(data.entries);

                // 初期ロード時は深さ制限を適用
                if (this.isInitialLoad && this.maxDepthLimit > 0) {
                    const filteredEntries = convertedEntries.filter(
                        (entry) => entry.depth <= this.maxDepthLimit,
                    );
                    this.openingBook.addEntries(filteredEntries);
                    console.log(
                        `📖 ${fileName}: ${filteredEntries.length}/${convertedEntries.length} エントリ追加（深さ${this.maxDepthLimit}まで）`,
                    );
                } else {
                    this.openingBook.addEntries(convertedEntries);
                    console.log(`📖 ${fileName}: ${convertedEntries.length} エントリ追加`);
                }
            } else {
                // 通常のJSON形式として処理
                return super.processResponse(response, fileName);
            }
        } catch (error) {
            console.error(`❌ ${fileName} 読み込みエラー:`, error);
        }
    }

    /**
     * MessagePack形式かどうかを判定
     */
    private isMsgPack(data: Uint8Array): boolean {
        if (data.length < 1) return false;

        // MessagePackのマジックバイト
        const firstByte = data[0];
        return (
            // fixmap (0x80-0x8f)
            (firstByte >= 0x80 && firstByte <= 0x8f) ||
            // fixarray (0x90-0x9f)
            (firstByte >= 0x90 && firstByte <= 0x9f) ||
            // map 16/32
            firstByte === 0xde ||
            firstByte === 0xdf ||
            // array 16/32
            firstByte === 0xdc ||
            firstByte === 0xdd
        );
    }

    /**
     * バイナリエントリーを通常の形式に変換
     */
    private convertBinaryEntries(entries: OpeningEntryBinary[]): OpeningEntry[] {
        return entries.map((entry) => ({
            position: entry.positionHash,
            depth: entry.depth,
            moves: entry.moves.map((move) => this.convertBinaryMove(move)),
        }));
    }

    /**
     * バイナリ形式の手を通常の形式に変換
     */
    private convertBinaryMove(moveBin: MoveBinary): OpeningMove {
        const pieceType = this.getPieceTypeFromBinary(moveBin.flags & MoveFlags.PIECE_MASK);
        const isPromote = (moveBin.flags & MoveFlags.PROMOTE) !== 0;
        const isDrop = (moveBin.flags & MoveFlags.DROP) !== 0;

        let move: Move;

        if (isDrop) {
            const to = indexToCoordinate(moveBin.to);
            if (!to) throw new Error(`Invalid drop position: ${moveBin.to}`);

            move = {
                type: "drop",
                to: { row: to.row as Row, column: to.column as Column },
                piece: {
                    type: pieceType,
                    owner: "black", // 定跡では手番から推測する必要がある
                    promoted: false,
                },
            };
        } else {
            const from = indexToCoordinate(moveBin.from);
            const to = indexToCoordinate(moveBin.to);
            if (!from || !to) throw new Error(`Invalid move: ${moveBin.from}-${moveBin.to}`);

            move = {
                type: "move",
                from: { row: from.row as Row, column: from.column as Column },
                to: { row: to.row as Row, column: to.column as Column },
                piece: {
                    type: pieceType,
                    owner: "black", // 定跡では手番から推測する必要がある
                    promoted: this.isPromotedPiece(moveBin.flags & MoveFlags.PIECE_MASK),
                },
                promote: isPromote,
                captured: null, // 定跡では捕獲情報は保存していない
            };
        }

        return {
            move,
            weight: moveBin.weight,
            name: "定跡",
            comment: `評価値: ${moveBin.eval >= 0 ? "+" : ""}${moveBin.eval}`,
        };
    }

    /**
     * バイナリ値から駒種類を取得
     */
    private getPieceTypeFromBinary(value: number): PieceType {
        const mapping: Record<number, PieceType> = {
            [PieceTypeBinary.PAWN]: "pawn",
            [PieceTypeBinary.LANCE]: "lance",
            [PieceTypeBinary.KNIGHT]: "knight",
            [PieceTypeBinary.SILVER]: "silver",
            [PieceTypeBinary.GOLD]: "gold",
            [PieceTypeBinary.BISHOP]: "bishop",
            [PieceTypeBinary.ROOK]: "rook",
            [PieceTypeBinary.KING]: "king",
            [PieceTypeBinary.PROMOTED_PAWN]: "pawn",
            [PieceTypeBinary.PROMOTED_LANCE]: "lance",
            [PieceTypeBinary.PROMOTED_KNIGHT]: "knight",
            [PieceTypeBinary.PROMOTED_SILVER]: "silver",
            [PieceTypeBinary.PROMOTED_BISHOP]: "bishop",
            [PieceTypeBinary.PROMOTED_ROOK]: "rook",
        };

        return mapping[value] || ("pawn" as PieceType);
    }

    /**
     * 成駒かどうかを判定
     */
    private isPromotedPiece(value: number): boolean {
        return value >= PieceTypeBinary.PROMOTED_PAWN;
    }
}

/**
 * シングルトンインスタンス管理（バイナリ形式対応）
 */
let globalBinaryLoader: BinaryOpeningBookLoader | null = null;

export async function getBinaryOpeningBookLoader(
    baseUrl: string,
): Promise<BinaryOpeningBookLoader> {
    if (!globalBinaryLoader) {
        globalBinaryLoader = new BinaryOpeningBookLoader(baseUrl);
        await globalBinaryLoader.initialize();
    }
    return globalBinaryLoader;
}
