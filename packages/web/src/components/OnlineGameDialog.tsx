import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogHeader,
    DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Check, Copy } from "lucide-react";
import { useState } from "react";

interface OnlineGameDialogProps {
    open: boolean;
    onOpenChange: (open: boolean) => void;
    onCreateHost: (playerName: string) => Promise<string>;
    onJoinAsGuest: (offer: string, playerName: string) => Promise<string>;
    onAcceptAnswer: (answer: string) => Promise<void>;
}

export function OnlineGameDialog({
    open,
    onOpenChange,
    onCreateHost,
    onJoinAsGuest,
    onAcceptAnswer,
}: OnlineGameDialogProps) {
    const [activeTab, setActiveTab] = useState<"host" | "guest">("host");
    const [offer, setOffer] = useState("");
    const [answer, setAnswer] = useState("");
    const [guestOffer, setGuestOffer] = useState("");
    const [hostAnswer, setHostAnswer] = useState("");
    const [hostName, setHostName] = useState("");
    const [guestName, setGuestName] = useState("");
    const [copied, setCopied] = useState(false);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState("");
    const [step, setStep] = useState<"create" | "wait" | "complete">("create");

    const handleCreateHost = async () => {
        if (!hostName.trim()) {
            setError("プレイヤー名を入力してください");
            return;
        }

        setLoading(true);
        setError("");
        try {
            const offerData = await onCreateHost(hostName.trim());
            setOffer(offerData);
            setStep("wait");
        } catch (err) {
            setError("ホストの作成に失敗しました");
            console.error(err);
        } finally {
            setLoading(false);
        }
    };

    const handleJoinAsGuest = async () => {
        if (!guestOffer.trim()) {
            setError("オファーを入力してください");
            return;
        }

        if (!guestName.trim()) {
            setError("プレイヤー名を入力してください");
            return;
        }

        setLoading(true);
        setError("");
        try {
            const answerData = await onJoinAsGuest(guestOffer, guestName.trim());
            setAnswer(answerData);
            setStep("wait");
        } catch (err) {
            setError("接続に失敗しました");
            console.error(err);
        } finally {
            setLoading(false);
        }
    };

    const handleAcceptAnswer = async () => {
        if (!hostAnswer.trim()) {
            setError("アンサーを入力してください");
            return;
        }

        setLoading(true);
        setError("");
        try {
            await onAcceptAnswer(hostAnswer);
            setStep("complete");
            setTimeout(() => {
                onOpenChange(false);
            }, 1500);
        } catch (err) {
            setError("アンサーの受け入れに失敗しました");
            console.error(err);
        } finally {
            setLoading(false);
        }
    };

    const copyToClipboard = (text: string) => {
        navigator.clipboard.writeText(text);
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
    };

    const resetDialog = () => {
        setActiveTab("host");
        setOffer("");
        setAnswer("");
        setGuestOffer("");
        setHostAnswer("");
        setHostName("");
        setGuestName("");
        setError("");
        setStep("create");
    };

    return (
        <Dialog
            open={open}
            onOpenChange={(open) => {
                onOpenChange(open);
                if (!open) resetDialog();
            }}
        >
            <DialogContent className="sm:max-w-[500px]">
                <DialogHeader>
                    <DialogTitle>通信対戦を開始</DialogTitle>
                    <DialogDescription>相手と接続情報を交換して対戦を開始します</DialogDescription>
                </DialogHeader>

                {error && (
                    <Alert variant="destructive">
                        <AlertDescription>{error}</AlertDescription>
                    </Alert>
                )}

                <Tabs value={activeTab} onValueChange={(v) => setActiveTab(v as "host" | "guest")}>
                    <TabsList className="grid w-full grid-cols-2">
                        <TabsTrigger value="host" disabled={step !== "create"}>
                            部屋を作る
                        </TabsTrigger>
                        <TabsTrigger value="guest" disabled={step !== "create"}>
                            部屋に参加
                        </TabsTrigger>
                    </TabsList>

                    <TabsContent value="host" className="space-y-4">
                        {step === "create" && (
                            <div className="space-y-4">
                                <p className="text-sm text-muted-foreground">
                                    部屋を作成して、接続情報を相手に送信してください
                                </p>
                                <div>
                                    <label
                                        htmlFor="host-player-name"
                                        className="text-sm font-medium"
                                    >
                                        プレイヤー名
                                    </label>
                                    <Input
                                        id="host-player-name"
                                        value={hostName}
                                        onChange={(e) => setHostName(e.target.value)}
                                        placeholder="あなたの名前を入力"
                                        className="mt-1"
                                    />
                                </div>
                                <Button
                                    onClick={handleCreateHost}
                                    disabled={loading}
                                    className="w-full"
                                >
                                    {loading ? "作成中..." : "部屋を作成"}
                                </Button>
                            </div>
                        )}

                        {step === "wait" && (
                            <div className="space-y-4">
                                <div>
                                    <div className="text-sm font-medium">
                                        1. この接続情報を相手に送信してください
                                    </div>
                                    <div className="mt-2 flex gap-2">
                                        <Input
                                            value={offer}
                                            readOnly
                                            className="font-mono text-xs"
                                        />
                                        <Button
                                            size="icon"
                                            variant="outline"
                                            onClick={() => copyToClipboard(offer)}
                                        >
                                            {copied ? (
                                                <Check className="h-4 w-4" />
                                            ) : (
                                                <Copy className="h-4 w-4" />
                                            )}
                                        </Button>
                                    </div>
                                </div>

                                <div>
                                    <div className="text-sm font-medium">
                                        2. 相手からの返答を入力してください
                                    </div>
                                    <div className="mt-2 space-y-2">
                                        <Input
                                            value={hostAnswer}
                                            onChange={(e) => setHostAnswer(e.target.value)}
                                            placeholder="相手からの返答を貼り付け"
                                            className="font-mono text-xs"
                                        />
                                        <Button
                                            onClick={handleAcceptAnswer}
                                            disabled={loading || !hostAnswer.trim()}
                                            className="w-full"
                                        >
                                            {loading ? "接続中..." : "接続を完了"}
                                        </Button>
                                    </div>
                                </div>
                            </div>
                        )}

                        {step === "complete" && (
                            <div className="text-center py-4">
                                <Check className="h-12 w-12 text-green-600 mx-auto mb-2" />
                                <p className="text-green-600 font-medium">接続完了！</p>
                                <p className="text-sm text-muted-foreground mt-1">
                                    まもなく対戦が開始されます
                                </p>
                            </div>
                        )}
                    </TabsContent>

                    <TabsContent value="guest" className="space-y-4">
                        {step === "create" && (
                            <div className="space-y-4">
                                <div>
                                    <div className="text-sm font-medium">
                                        ホストから受け取った接続情報を入力
                                    </div>
                                    <div className="mt-2 space-y-2">
                                        <Input
                                            value={guestOffer}
                                            onChange={(e) => setGuestOffer(e.target.value)}
                                            placeholder="接続情報を貼り付け"
                                            className="font-mono text-xs"
                                        />
                                    </div>
                                </div>
                                <div>
                                    <label
                                        htmlFor="guest-player-name"
                                        className="text-sm font-medium"
                                    >
                                        プレイヤー名
                                    </label>
                                    <Input
                                        id="guest-player-name"
                                        value={guestName}
                                        onChange={(e) => setGuestName(e.target.value)}
                                        placeholder="あなたの名前を入力"
                                        className="mt-1"
                                    />
                                </div>
                                <Button
                                    onClick={handleJoinAsGuest}
                                    disabled={loading || !guestOffer.trim()}
                                    className="w-full"
                                >
                                    {loading ? "接続中..." : "部屋に参加"}
                                </Button>
                            </div>
                        )}

                        {step === "wait" && (
                            <div className="space-y-4">
                                <div>
                                    <div className="text-sm font-medium">
                                        この返答をホストに送信してください
                                    </div>
                                    <div className="mt-2 flex gap-2">
                                        <Input
                                            value={answer}
                                            readOnly
                                            className="font-mono text-xs"
                                        />
                                        <Button
                                            size="icon"
                                            variant="outline"
                                            onClick={() => copyToClipboard(answer)}
                                        >
                                            {copied ? (
                                                <Check className="h-4 w-4" />
                                            ) : (
                                                <Copy className="h-4 w-4" />
                                            )}
                                        </Button>
                                    </div>
                                </div>
                                <p className="text-sm text-muted-foreground">
                                    ホストが返答を受け入れると接続が完了します
                                </p>
                            </div>
                        )}
                    </TabsContent>
                </Tabs>
            </DialogContent>
        </Dialog>
    );
}
