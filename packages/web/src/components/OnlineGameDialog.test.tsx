import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { OnlineGameDialog } from "./OnlineGameDialog";

describe("OnlineGameDialog", () => {
    const mockOnOpenChange = vi.fn();
    const mockOnCreateHost = vi.fn();
    const mockOnJoinAsGuest = vi.fn();
    const mockOnAcceptAnswer = vi.fn();
    const mockWriteText = vi.fn();

    const defaultProps = {
        open: true,
        onOpenChange: mockOnOpenChange,
        onCreateHost: mockOnCreateHost,
        onJoinAsGuest: mockOnJoinAsGuest,
        onAcceptAnswer: mockOnAcceptAnswer,
    };

    beforeEach(() => {
        vi.clearAllMocks();
        vi.spyOn(console, "error").mockImplementation(() => {});
        // navigator.clipboard のモック
        Object.defineProperty(navigator, "clipboard", {
            value: {
                writeText: mockWriteText,
            },
            writable: true,
            configurable: true,
        });
    });

    afterEach(() => {
        vi.clearAllTimers();
        vi.restoreAllMocks();
    });

    describe("基本的な動作", () => {
        it("ダイアログが表示される", () => {
            render(<OnlineGameDialog {...defaultProps} />);
            expect(screen.getByText("通信対戦を開始")).toBeInTheDocument();
            expect(
                screen.getByText("相手と接続情報を交換して対戦を開始します"),
            ).toBeInTheDocument();
        });

        it("閉じるときにresetDialogが呼ばれる", () => {
            const { rerender } = render(<OnlineGameDialog {...defaultProps} />);

            // 何か入力する
            fireEvent.change(screen.getByPlaceholderText("あなたの名前を入力"), {
                target: { value: "テストプレイヤー" },
            });

            // ダイアログを閉じる
            const closeButton = screen.getByRole("button", { name: /close/i });
            fireEvent.click(closeButton);

            expect(mockOnOpenChange).toHaveBeenCalledWith(false);
        });

        it("タブの切り替えができる", async () => {
            render(<OnlineGameDialog {...defaultProps} />);

            // デフォルトはホストタブ
            expect(
                screen.getByText("部屋を作成して、接続情報を相手に送信してください"),
            ).toBeInTheDocument();

            // ゲストタブに切り替え
            const guestTab = screen.getByRole("tab", { name: "部屋に参加" });
            fireEvent.click(guestTab);

            await waitFor(() => {
                const guestInput = screen.getAllByPlaceholderText("あなたの名前を入力");
                expect(guestInput.length).toBeGreaterThan(0);
            });
        });
    });

    describe("ホスト機能", () => {
        it("プレイヤー名が未入力の場合、エラーが表示される", async () => {
            render(<OnlineGameDialog {...defaultProps} />);

            fireEvent.click(screen.getByText("部屋を作成"));

            await waitFor(() => {
                expect(screen.getByText("プレイヤー名を入力してください")).toBeInTheDocument();
            });
            expect(mockOnCreateHost).not.toHaveBeenCalled();
        });

        it("ホストの作成が成功する", async () => {
            mockOnCreateHost.mockResolvedValue("test-offer-data");
            render(<OnlineGameDialog {...defaultProps} />);

            fireEvent.change(screen.getByPlaceholderText("あなたの名前を入力"), {
                target: { value: "ホストプレイヤー" },
            });

            fireEvent.click(screen.getByText("部屋を作成"));

            await waitFor(() => {
                expect(mockOnCreateHost).toHaveBeenCalledWith("ホストプレイヤー");
                expect(
                    screen.getByText("1. この接続情報を相手に送信してください"),
                ).toBeInTheDocument();
                expect(screen.getByDisplayValue("test-offer-data")).toBeInTheDocument();
            });
        });

        it("ホストの作成が失敗する", async () => {
            mockOnCreateHost.mockRejectedValue(new Error("WebRTC初期化エラー"));
            render(<OnlineGameDialog {...defaultProps} />);

            fireEvent.change(screen.getByPlaceholderText("あなたの名前を入力"), {
                target: { value: "ホストプレイヤー" },
            });

            fireEvent.click(screen.getByText("部屋を作成"));

            await waitFor(() => {
                expect(screen.getByText("WebRTC初期化エラー")).toBeInTheDocument();
            });
        });

        it("オファーをクリップボードにコピーできる", async () => {
            mockOnCreateHost.mockResolvedValue("test-offer-data");
            mockWriteText.mockResolvedValue(undefined);

            render(<OnlineGameDialog {...defaultProps} />);

            fireEvent.change(screen.getByPlaceholderText("あなたの名前を入力"), {
                target: { value: "ホストプレイヤー" },
            });

            fireEvent.click(screen.getByText("部屋を作成"));

            await waitFor(() => {
                expect(screen.getByDisplayValue("test-offer-data")).toBeInTheDocument();
            });

            const copyButtons = screen.getAllByRole("button");
            const copyButton = copyButtons.find((btn) => {
                // Lucide React iconを含むボタンを探す
                return btn.querySelector('[class*="lucide"]') || btn.querySelector("svg");
            });
            if (copyButton) {
                fireEvent.click(copyButton);
            }

            await waitFor(() => {
                expect(mockWriteText).toHaveBeenCalledWith("test-offer-data");
            });
        });

        it("アンサーの受け入れが成功する", async () => {
            mockOnCreateHost.mockResolvedValue("test-offer-data");
            mockOnAcceptAnswer.mockResolvedValue(undefined);

            const { unmount } = render(<OnlineGameDialog {...defaultProps} />);

            // ホストを作成
            fireEvent.change(screen.getByPlaceholderText("あなたの名前を入力"), {
                target: { value: "ホストプレイヤー" },
            });
            fireEvent.click(screen.getByText("部屋を作成"));

            await waitFor(() => {
                expect(screen.getByText("2. 相手からの返答を入力してください")).toBeInTheDocument();
            });

            // アンサーを入力
            fireEvent.change(screen.getByPlaceholderText("相手からの返答を貼り付け"), {
                target: { value: "test-answer-data" },
            });

            fireEvent.click(screen.getByText("接続を完了"));

            await waitFor(() => {
                expect(mockOnAcceptAnswer).toHaveBeenCalledWith("test-answer-data");
                expect(screen.getByText("接続完了！")).toBeInTheDocument();
            });

            // 1.5秒後にダイアログが閉じる
            await waitFor(
                () => {
                    expect(mockOnOpenChange).toHaveBeenCalledWith(false);
                },
                { timeout: 2000 },
            );

            // テスト終了時にコンポーネントをアンマウントして、タイマーをクリア
            unmount();
        });
    });

    describe("ゲスト機能", () => {
        it("オファーが未入力の場合、ボタンが無効化される", async () => {
            const user = userEvent.setup();
            render(<OnlineGameDialog {...defaultProps} />);

            // ゲストタブをクリックしてタブを切り替える
            const guestTab = screen.getByRole("tab", { name: "部屋に参加" });
            await user.click(guestTab);

            // TabsContentが切り替わるまで待つ
            await waitFor(() => {
                // ゲストタブ固有のテキストが表示されていることを確認
                expect(screen.getByText("ホストから受け取った接続情報を入力")).toBeInTheDocument();
            });

            // 名前を入力（オファーは空のまま）
            const nameInput = screen.getByPlaceholderText("あなたの名前を入力");
            await user.type(nameInput, "ゲストプレイヤー");

            // 部屋に参加ボタンが無効化されていることを確認
            const joinButton = screen.getByRole("button", { name: "部屋に参加" });
            expect(joinButton).toBeDisabled();

            // クリックしてもonJoinAsGuestが呼ばれないことを確認
            await user.click(joinButton);
            expect(mockOnJoinAsGuest).not.toHaveBeenCalled();
        });

        it("プレイヤー名が未入力の場合、エラーが表示される", async () => {
            const user = userEvent.setup();
            render(<OnlineGameDialog {...defaultProps} />);

            // ゲストタブをクリック
            const guestTab = screen.getByRole("tab", { name: "部屋に参加" });
            await user.click(guestTab);

            // TabsContentが切り替わるまで待つ
            await waitFor(() => {
                expect(screen.getByText("ホストから受け取った接続情報を入力")).toBeInTheDocument();
            });

            // オファーを入力（名前は空のまま）
            const offerInput = screen.getByPlaceholderText("接続情報を貼り付け");
            await user.type(offerInput, "test-offer-data");

            // 部屋に参加ボタンをクリック
            const joinButton = screen.getByRole("button", { name: "部屋に参加" });
            await user.click(joinButton);

            await waitFor(() => {
                expect(screen.getByText("プレイヤー名を入力してください")).toBeInTheDocument();
            });
            expect(mockOnJoinAsGuest).not.toHaveBeenCalled();
        });

        it("ゲストとしての参加が成功する", async () => {
            mockOnJoinAsGuest.mockResolvedValue("test-answer-data");
            const user = userEvent.setup();
            render(<OnlineGameDialog {...defaultProps} />);

            // ゲストタブをクリック
            const guestTab = screen.getByRole("tab", { name: "部屋に参加" });
            await user.click(guestTab);

            // TabsContentが切り替わるまで待つ
            await waitFor(() => {
                expect(screen.getByText("ホストから受け取った接続情報を入力")).toBeInTheDocument();
            });

            // オファーを入力
            const offerInput = screen.getByPlaceholderText("接続情報を貼り付け");
            await user.type(offerInput, "test-offer-data");

            // 名前を入力
            const nameInput = screen.getByPlaceholderText("あなたの名前を入力");
            await user.type(nameInput, "ゲストプレイヤー");

            // 部屋に参加ボタンをクリック
            const joinButton = screen.getByRole("button", { name: "部屋に参加" });
            await user.click(joinButton);

            await waitFor(() => {
                expect(mockOnJoinAsGuest).toHaveBeenCalledWith(
                    "test-offer-data",
                    "ゲストプレイヤー",
                );
                expect(screen.getByText("この返答をホストに送信してください")).toBeInTheDocument();
                expect(screen.getByDisplayValue("test-answer-data")).toBeInTheDocument();
            });
        });

        it("ゲストとしての参加が失敗する", async () => {
            mockOnJoinAsGuest.mockRejectedValue(new Error("無効なオファーデータ"));
            const user = userEvent.setup();
            render(<OnlineGameDialog {...defaultProps} />);

            // ゲストタブをクリック
            const guestTab = screen.getByRole("tab", { name: "部屋に参加" });
            await user.click(guestTab);

            // TabsContentが切り替わるまで待つ
            await waitFor(() => {
                expect(screen.getByText("ホストから受け取った接続情報を入力")).toBeInTheDocument();
            });

            // オファーを入力
            const offerInput = screen.getByPlaceholderText("接続情報を貼り付け");
            await user.type(offerInput, "invalid-offer");

            // 名前を入力
            const nameInput = screen.getByPlaceholderText("あなたの名前を入力");
            await user.type(nameInput, "ゲストプレイヤー");

            // 部屋に参加ボタンをクリック
            const joinButton = screen.getByRole("button", { name: "部屋に参加" });
            await user.click(joinButton);

            await waitFor(() => {
                expect(screen.getByText("無効なオファーデータ")).toBeInTheDocument();
            });
        });
    });

    describe("エラーハンドリング", () => {
        it("クリップボードへのコピーが失敗した場合、エラーが表示される", async () => {
            mockOnCreateHost.mockResolvedValue("test-offer-data");
            mockWriteText.mockRejectedValue(new Error("Clipboard access denied"));

            const { unmount } = render(<OnlineGameDialog {...defaultProps} />);

            fireEvent.change(screen.getByPlaceholderText("あなたの名前を入力"), {
                target: { value: "ホストプレイヤー" },
            });
            fireEvent.click(screen.getByText("部屋を作成"));

            await waitFor(() => {
                expect(screen.getByDisplayValue("test-offer-data")).toBeInTheDocument();
            });

            const copyButtons = screen.getAllByRole("button");
            const copyButton = copyButtons.find((btn) => {
                // Lucide React iconを含むボタンを探す
                return btn.querySelector('[class*="lucide"]') || btn.querySelector("svg");
            });
            if (copyButton) {
                fireEvent.click(copyButton);
            }

            await waitFor(() => {
                expect(mockWriteText).toHaveBeenCalledWith("test-offer-data");
            });

            await waitFor(
                () => {
                    expect(
                        screen.getByText("クリップボードへのコピーに失敗しました"),
                    ).toBeInTheDocument();
                },
                { timeout: 3000 },
            );

            unmount();
        });

        it("ローディング中はボタンが無効化される", async () => {
            mockOnCreateHost.mockImplementation(() => new Promise(() => {})); // 永続的にpending

            render(<OnlineGameDialog {...defaultProps} />);

            fireEvent.change(screen.getByPlaceholderText("あなたの名前を入力"), {
                target: { value: "ホストプレイヤー" },
            });

            const createButton = screen.getByText("部屋を作成");
            fireEvent.click(createButton);

            await waitFor(() => {
                expect(screen.getByText("作成中...")).toBeDisabled();
            });
        });

        it("step !== 'create'の時、タブが無効化される", async () => {
            mockOnCreateHost.mockResolvedValue("test-offer-data");
            const { unmount } = render(<OnlineGameDialog {...defaultProps} />);

            fireEvent.change(screen.getByPlaceholderText("あなたの名前を入力"), {
                target: { value: "ホストプレイヤー" },
            });

            fireEvent.click(screen.getByText("部屋を作成"));

            await waitFor(() => {
                expect(screen.getByText("部屋を作る")).toBeDisabled();
                expect(screen.getByText("部屋に参加")).toBeDisabled();
            });

            // クリーンアップ
            unmount();
        });
    });

    describe("接続状態の管理", () => {
        it("ゲストが接続されると自動的に完了状態に移行する", async () => {
            const user = userEvent.setup();
            mockOnJoinAsGuest.mockResolvedValueOnce("mock-answer-data");

            const { rerender } = render(<OnlineGameDialog {...defaultProps} isConnected={false} />);

            // ゲストタブに切り替え
            const guestTab = screen.getByRole("tab", { name: "部屋に参加" });
            await user.click(guestTab);

            // ゲスト情報を入力して接続
            const offerInput = screen.getByPlaceholderText("接続情報を貼り付け");
            await user.type(offerInput, "mock-offer-data");

            const nameInput = screen.getByPlaceholderText("あなたの名前を入力");
            await user.type(nameInput, "ゲストプレイヤー");

            const joinButton = screen.getByRole("button", { name: "部屋に参加" });
            await user.click(joinButton);

            // 待機状態になることを確認
            await waitFor(() => {
                expect(screen.getByText("この返答をホストに送信してください")).toBeInTheDocument();
            });

            // 接続が確立された状態で再レンダリング
            rerender(<OnlineGameDialog {...defaultProps} isConnected={true} />);

            // 完了状態に移行することを確認
            await waitFor(() => {
                expect(screen.getByText("接続完了！")).toBeInTheDocument();
                expect(screen.getByText("まもなく対戦が開始されます")).toBeInTheDocument();
            });
        });

        it("ホストの場合は接続状態が変わっても自動移行しない（手動完了のため）", async () => {
            const user = userEvent.setup();
            mockOnCreateHost.mockResolvedValueOnce("mock-offer-data");

            const { rerender } = render(<OnlineGameDialog {...defaultProps} isConnected={false} />);

            // ホスト情報を入力して部屋作成
            const nameInput = screen.getByPlaceholderText("あなたの名前を入力");
            await user.type(nameInput, "ホストプレイヤー");

            const createButton = screen.getByText("部屋を作成");
            await user.click(createButton);

            // 待機状態になることを確認
            await waitFor(() => {
                expect(
                    screen.getByText("1. この接続情報を相手に送信してください"),
                ).toBeInTheDocument();
            });

            // 接続が確立された状態で再レンダリング
            rerender(<OnlineGameDialog {...defaultProps} isConnected={true} />);

            // ホストの場合は自動移行せず、待機状態のままであることを確認
            expect(screen.getByText("2. 相手からの返答を入力してください")).toBeInTheDocument();
            expect(screen.queryByText("接続完了！")).not.toBeInTheDocument();
        });
    });
});
