import { useState } from "react";
import { useP2P } from "../hooks/useP2P";
import { ConnectionDialog } from "./ConnectionDialog";
import { Button } from "./ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";

export function P2PTest() {
    const { isConnected, sendMessage, receivedMessages } = useP2P();
    const [testMessage, setTestMessage] = useState("");

    const handleSendTest = () => {
        const message = testMessage || `Test message ${Date.now()}`;
        sendMessage(message);
        setTestMessage("");
    };

    return (
        <div className="container mx-auto p-4 max-w-4xl">
            <h1 className="text-2xl font-bold mb-6">サーバーレスP2Pテスト</h1>

            <div className="grid gap-6 md:grid-cols-2">
                <div>
                    <ConnectionDialog />
                </div>

                <div className="space-y-4">
                    <Card>
                        <CardHeader>
                            <CardTitle>接続状態</CardTitle>
                        </CardHeader>
                        <CardContent>
                            <p className="text-sm">
                                接続状態: {isConnected ? "🟢 接続済み" : "🔴 未接続"}
                            </p>
                        </CardContent>
                    </Card>

                    {isConnected && (
                        <Card>
                            <CardHeader>
                                <CardTitle>メッセージテスト</CardTitle>
                            </CardHeader>
                            <CardContent className="space-y-4">
                                <div className="flex gap-2">
                                    <input
                                        type="text"
                                        value={testMessage}
                                        onChange={(e) => setTestMessage(e.target.value)}
                                        placeholder="テストメッセージ"
                                        className="flex-1 px-3 py-2 border rounded"
                                    />
                                    <Button onClick={handleSendTest}>送信</Button>
                                </div>

                                <div>
                                    <h4 className="font-semibold mb-2">受信メッセージ:</h4>
                                    <div className="max-h-40 overflow-y-auto bg-gray-50 rounded p-2">
                                        {receivedMessages.length === 0 ? (
                                            <p className="text-gray-500 text-sm">
                                                まだメッセージがありません
                                            </p>
                                        ) : (
                                            receivedMessages.map((msg, index) => (
                                                <div
                                                    key={`msg-${index}-${msg.substring(0, 10)}`}
                                                    className="text-sm"
                                                >
                                                    {msg}
                                                </div>
                                            ))
                                        )}
                                    </div>
                                </div>
                            </CardContent>
                        </Card>
                    )}
                </div>
            </div>
        </div>
    );
}
