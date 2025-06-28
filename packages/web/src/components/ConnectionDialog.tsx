import { Copy, Loader2 } from "lucide-react";
import { useState } from "react";
import { useP2P } from "../hooks/useP2P";
import { Button } from "./ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./ui/card";
import { Input } from "./ui/input";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "./ui/tabs";

export function ConnectionDialog() {
    const {
        isInitialized,
        isConnected,
        connectionMode,
        hostTicket,
        createHost,
        joinWithTicket,
        disconnect,
    } = useP2P();

    const [joinTicket, setJoinTicket] = useState("");
    const [copied, setCopied] = useState(false);

    const handleCopyTicket = async () => {
        if (hostTicket) {
            await navigator.clipboard.writeText(hostTicket);
            setCopied(true);
            setTimeout(() => setCopied(false), 2000);
        }
    };

    const handleJoin = () => {
        if (joinTicket.trim()) {
            joinWithTicket(joinTicket.trim());
        }
    };

    if (!isInitialized) {
        return (
            <Card className="w-full max-w-md mx-auto">
                <CardContent className="flex items-center justify-center py-8">
                    <Loader2 className="h-6 w-6 animate-spin" />
                    <span className="ml-2">P2P接続を初期化中...</span>
                </CardContent>
            </Card>
        );
    }

    if (isConnected) {
        return (
            <Card className="w-full max-w-md mx-auto">
                <CardHeader>
                    <CardTitle>P2P接続済み</CardTitle>
                    <CardDescription>
                        接続モード: {connectionMode === "serverless" ? "サーバーレス" : "ローカル"}
                    </CardDescription>
                </CardHeader>
                <CardContent>
                    <Button onClick={disconnect} variant="destructive" className="w-full">
                        切断
                    </Button>
                </CardContent>
            </Card>
        );
    }

    return (
        <Card className="w-full max-w-md mx-auto">
            <CardHeader>
                <CardTitle>P2P対戦</CardTitle>
                <CardDescription>サーバーレスP2P接続で対戦相手と接続します</CardDescription>
            </CardHeader>
            <CardContent>
                <Tabs defaultValue="host" className="w-full">
                    <TabsList className="grid w-full grid-cols-2">
                        <TabsTrigger value="host">ホストとして開始</TabsTrigger>
                        <TabsTrigger value="join">チケットで参加</TabsTrigger>
                    </TabsList>

                    <TabsContent value="host" className="space-y-4">
                        {!hostTicket ? (
                            <div className="space-y-4">
                                <p className="text-sm text-muted-foreground">
                                    部屋を作成してチケットを相手に共有してください
                                </p>
                                <Button onClick={createHost} className="w-full">
                                    部屋を作成
                                </Button>
                            </div>
                        ) : (
                            <div className="space-y-4">
                                <p className="text-sm text-muted-foreground">
                                    このチケットを相手に共有してください：
                                </p>
                                <div className="flex gap-2">
                                    <Input
                                        value={hostTicket}
                                        readOnly
                                        className="font-mono text-xs"
                                    />
                                    <Button
                                        size="icon"
                                        variant="outline"
                                        onClick={handleCopyTicket}
                                    >
                                        <Copy className="h-4 w-4" />
                                    </Button>
                                </div>
                                {copied && (
                                    <p className="text-sm text-green-600">コピーしました！</p>
                                )}
                                <p className="text-sm text-muted-foreground">
                                    相手の接続を待っています...
                                </p>
                            </div>
                        )}
                    </TabsContent>

                    <TabsContent value="join" className="space-y-4">
                        <p className="text-sm text-muted-foreground">
                            ホストから共有されたチケットを入力してください
                        </p>
                        <Input
                            value={joinTicket}
                            onChange={(e) => setJoinTicket(e.target.value)}
                            placeholder="チケットを貼り付け..."
                            className="font-mono text-xs"
                        />
                        <Button
                            onClick={handleJoin}
                            disabled={!joinTicket.trim()}
                            className="w-full"
                        >
                            接続
                        </Button>
                    </TabsContent>
                </Tabs>
            </CardContent>
        </Card>
    );
}
