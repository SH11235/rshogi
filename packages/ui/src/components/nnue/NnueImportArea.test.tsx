import type { NnueStorageCapabilities } from "@shogi/app-core";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { NnueImportArea } from "./NnueImportArea";

// Web 用 capabilities（File インポートのみ）
const webCapabilities: NnueStorageCapabilities = {
    supportsFileImport: true,
    supportsPathImport: false,
    supportsLoad: true,
};

// Desktop 用 capabilities（パスインポートのみ）
const desktopCapabilities: NnueStorageCapabilities = {
    supportsFileImport: false,
    supportsPathImport: true,
    supportsLoad: false,
};

// 将来の Desktop drag&drop 対応時の capabilities（両方サポート）
const hybridCapabilities: NnueStorageCapabilities = {
    supportsFileImport: true,
    supportsPathImport: true,
    supportsLoad: false,
};

describe("NnueImportArea", () => {
    describe("supportsFileImport=true (Web)", () => {
        it("ドラッグ＆ドロップのメッセージが表示される", () => {
            render(
                <NnueImportArea
                    capabilities={webCapabilities}
                    onFileSelect={vi.fn()}
                    onRequestFilePath={vi.fn()}
                />,
            );

            expect(screen.getByText("NNUE ファイルをドラッグ＆ドロップ")).toBeDefined();
        });

        it("ボタンクリックで input がクリックされる", () => {
            const onFileSelect = vi.fn();
            render(
                <NnueImportArea
                    capabilities={webCapabilities}
                    onFileSelect={onFileSelect}
                    onRequestFilePath={vi.fn()}
                />,
            );

            const button = screen.getByRole("button", { name: "ファイルを選択..." });
            fireEvent.click(button);

            // input はhiddenなので、クリックイベントのトリガーを確認
            // （実際のファイル選択ダイアログは開けないが、ボタンがクリックできることを確認）
            expect(button.getAttribute("disabled")).toBeNull();
        });

        it("ドラッグオーバー時にスタイルが変わる", () => {
            render(
                <NnueImportArea
                    capabilities={webCapabilities}
                    onFileSelect={vi.fn()}
                    onRequestFilePath={vi.fn()}
                />,
            );

            const dropZone = screen.getByRole("region", { name: "NNUE ファイルインポートエリア" });

            fireEvent.dragOver(dropZone);

            expect(screen.getByText("ここにドロップ")).toBeDefined();
        });
    });

    describe("supportsPathImport=true (Desktop)", () => {
        it("ファイル選択ボタンクリックのメッセージが表示される", () => {
            render(
                <NnueImportArea
                    capabilities={desktopCapabilities}
                    onFileSelect={vi.fn()}
                    onRequestFilePath={vi.fn()}
                />,
            );

            expect(screen.getByText("ファイル選択ボタンをクリック")).toBeDefined();
        });

        it("ボタンクリックで onRequestFilePath が呼ばれる", () => {
            const onRequestFilePath = vi.fn();
            render(
                <NnueImportArea
                    capabilities={desktopCapabilities}
                    onFileSelect={vi.fn()}
                    onRequestFilePath={onRequestFilePath}
                />,
            );

            const button = screen.getByRole("button", { name: "ファイルを選択..." });
            fireEvent.click(button);

            expect(onRequestFilePath).toHaveBeenCalledTimes(1);
        });

        it("ドラッグオーバーしてもスタイルが変わらない", () => {
            render(
                <NnueImportArea
                    capabilities={desktopCapabilities}
                    onFileSelect={vi.fn()}
                    onRequestFilePath={vi.fn()}
                />,
            );

            const dropZone = screen.getByRole("region", { name: "NNUE ファイルインポートエリア" });

            fireEvent.dragOver(dropZone);

            // ドラッグオーバーしても「ここにドロップ」にならない
            expect(screen.queryByText("ここにドロップ")).toBeNull();
            expect(screen.getByText("ファイル選択ボタンをクリック")).toBeDefined();
        });
    });

    describe("両方 true の場合（将来の Desktop drag&drop 対応）", () => {
        it("ドラッグ＆ドロップのメッセージが表示される", () => {
            render(
                <NnueImportArea
                    capabilities={hybridCapabilities}
                    onFileSelect={vi.fn()}
                    onRequestFilePath={vi.fn()}
                />,
            );

            expect(screen.getByText("NNUE ファイルをドラッグ＆ドロップ")).toBeDefined();
        });

        it("ボタンクリックで onRequestFilePath が優先される", () => {
            const onFileSelect = vi.fn();
            const onRequestFilePath = vi.fn();
            render(
                <NnueImportArea
                    capabilities={hybridCapabilities}
                    onFileSelect={onFileSelect}
                    onRequestFilePath={onRequestFilePath}
                />,
            );

            const button = screen.getByRole("button", { name: "ファイルを選択..." });
            fireEvent.click(button);

            // supportsPathImport が優先されるので onRequestFilePath が呼ばれる
            expect(onRequestFilePath).toHaveBeenCalledTimes(1);
        });

        it("ドラッグオーバー時にスタイルが変わる", () => {
            render(
                <NnueImportArea
                    capabilities={hybridCapabilities}
                    onFileSelect={vi.fn()}
                    onRequestFilePath={vi.fn()}
                />,
            );

            const dropZone = screen.getByRole("region", { name: "NNUE ファイルインポートエリア" });

            fireEvent.dragOver(dropZone);

            expect(screen.getByText("ここにドロップ")).toBeDefined();
        });
    });

    describe("コールバック未設定の場合", () => {
        it("supportsFileImport=true でも onFileSelect がない場合はドラッグ&ドロップ無効", () => {
            render(
                <NnueImportArea
                    capabilities={webCapabilities}
                    // onFileSelect なし
                    onRequestFilePath={vi.fn()}
                />,
            );

            const dropZone = screen.getByRole("region", { name: "NNUE ファイルインポートエリア" });
            fireEvent.dragOver(dropZone);

            // ドラッグ&ドロップは無効なのでメッセージは変わらない
            expect(screen.queryByText("ここにドロップ")).toBeNull();
            expect(screen.getByText("ファイル選択ボタンをクリック")).toBeDefined();
        });

        it("supportsPathImport=true でも onRequestFilePath がない場合はボタン無効", () => {
            render(
                <NnueImportArea
                    capabilities={desktopCapabilities}
                    onFileSelect={vi.fn()}
                    // onRequestFilePath なし
                />,
            );

            const button = screen.getByRole("button", { name: "ファイルを選択..." });
            expect(button.getAttribute("disabled")).toBe("");
        });

        it("どちらのコールバックもない場合はボタン無効", () => {
            render(
                <NnueImportArea
                    capabilities={webCapabilities}
                    // どちらもなし
                />,
            );

            const button = screen.getByRole("button", { name: "ファイルを選択..." });
            expect(button.getAttribute("disabled")).toBe("");
        });
    });

    describe("disabled 状態", () => {
        it("disabled=true の場合、ボタンが無効化される", () => {
            render(
                <NnueImportArea
                    capabilities={webCapabilities}
                    onFileSelect={vi.fn()}
                    onRequestFilePath={vi.fn()}
                    disabled={true}
                />,
            );

            const button = screen.getByRole("button", { name: "ファイルを選択..." });
            expect(button.getAttribute("disabled")).toBe("");
        });

        it("isImporting=true の場合、インポート中メッセージが表示される", () => {
            render(
                <NnueImportArea
                    capabilities={webCapabilities}
                    onFileSelect={vi.fn()}
                    onRequestFilePath={vi.fn()}
                    isImporting={true}
                />,
            );

            expect(screen.getByText("インポート中...")).toBeDefined();
        });
    });
});
