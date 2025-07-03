import { Button } from "@/components/ui/button";
import {
    Dialog,
    DialogContent,
    DialogDescription,
    DialogHeader,
    DialogTitle,
} from "@/components/ui/dialog";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { BookOpen, Brain, Target, Trophy } from "lucide-react";

interface AIGuideDialogProps {
    open: boolean;
    onOpenChange: (open: boolean) => void;
}

export function AIGuideDialog({ open, onOpenChange }: AIGuideDialogProps) {
    return (
        <Dialog open={open} onOpenChange={onOpenChange}>
            <DialogContent className="max-w-4xl max-h-[80vh]">
                <DialogHeader>
                    <DialogTitle className="text-2xl flex items-center gap-2">
                        <Brain className="w-6 h-6" />
                        AI対戦ガイド
                    </DialogTitle>
                    <DialogDescription>将棋AIとの対戦を楽しむためのガイドです</DialogDescription>
                </DialogHeader>

                <ScrollArea className="h-[60vh] pr-4">
                    <div className="space-y-6">
                        {/* 難易度セクション */}
                        <section>
                            <h3 className="text-lg font-semibold mb-3 flex items-center gap-2">
                                <Target className="w-5 h-5" />
                                難易度について
                            </h3>

                            <div className="space-y-4">
                                <DifficultyCard
                                    level="初心者"
                                    emoji="🌱"
                                    strength="将棋を始めたばかりの方"
                                    depth="2手先"
                                    time="約1秒"
                                    features={[
                                        "基本的な駒の動きを学べます",
                                        "たまにミスをすることがあります",
                                        "優しい相手として練習に最適",
                                    ]}
                                />

                                <DifficultyCard
                                    level="中級者"
                                    emoji="📘"
                                    strength="ルールを覚えた方"
                                    depth="4手先"
                                    time="約3秒"
                                    features={[
                                        "基本的な戦術を使います",
                                        "王の守りを意識します",
                                        "駒の連携を考えます",
                                    ]}
                                />

                                <DifficultyCard
                                    level="上級者"
                                    emoji="🏆"
                                    strength="戦術を理解している方"
                                    depth="6手先"
                                    time="約5秒"
                                    features={[
                                        "高度な手筋を使います",
                                        "序中終盤を意識します",
                                        "形勢判断が正確です",
                                    ]}
                                />

                                <DifficultyCard
                                    level="エキスパート"
                                    emoji="👑"
                                    strength="有段者レベル"
                                    depth="8手先"
                                    time="約10秒"
                                    features={[
                                        "最強設定です",
                                        "プロ級の手筋を使います",
                                        "ミスはほとんどしません",
                                    ]}
                                />
                            </div>
                        </section>

                        <Separator />

                        {/* AIの思考 */}
                        <section>
                            <h3 className="text-lg font-semibold mb-3 flex items-center gap-2">
                                <Brain className="w-5 h-5" />
                                AIの思考内容
                            </h3>

                            <div className="space-y-3">
                                <EvaluationItem
                                    title="駒の価値"
                                    description="歩=1点、飛車=10.4点など、駒の強さを数値化"
                                />
                                <EvaluationItem
                                    title="位置の評価"
                                    description="中央や敵陣に近い駒を高く評価"
                                />
                                <EvaluationItem
                                    title="王の安全性"
                                    description="守り駒の配置、王手の有無を考慮"
                                />
                                <EvaluationItem
                                    title="駒の活動性"
                                    description="動ける範囲が広い駒を高く評価"
                                />
                                <EvaluationItem
                                    title="駒の連携"
                                    description="複数の駒が協力している形を重視"
                                />
                            </div>
                        </section>

                        <Separator />

                        {/* 上達のコツ */}
                        <section>
                            <h3 className="text-lg font-semibold mb-3 flex items-center gap-2">
                                <Trophy className="w-5 h-5" />
                                上達のコツ
                            </h3>

                            <div className="space-y-2 text-sm">
                                <p>• まずは初心者レベルから始めて、徐々に難易度を上げましょう</p>
                                <p>• AIの指し手を観察して、良い手筋を学びましょう</p>
                                <p>• 対局後に棋譜を振り返り、ポイントとなった手を分析しましょう</p>
                                <p>• 詰み探索機能を使って、終盤力を鍛えましょう</p>
                                <p>• 同じ難易度で安定して勝てるようになったら、次のレベルへ</p>
                            </div>
                        </section>

                        <Separator />

                        {/* FAQ */}
                        <section>
                            <h3 className="text-lg font-semibold mb-3 flex items-center gap-2">
                                <BookOpen className="w-5 h-5" />
                                よくある質問
                            </h3>

                            <div className="space-y-3">
                                <FAQItem
                                    question="AIが強すぎて勝てません"
                                    answer="より低い難易度から始めてください。初心者レベルは学習に最適です。"
                                />
                                <FAQItem
                                    question="AIの手が理解できません"
                                    answer="AIは評価関数に基づいて最善手を選んでいます。対局後に手順を振り返ってみましょう。"
                                />
                                <FAQItem
                                    question="同じ局面で違う手を指すことはありますか？"
                                    answer="初心者レベルでは30%の確率でランダムな手を指します。他のレベルでは基本的に同じ手を指します。"
                                />
                            </div>
                        </section>
                    </div>
                </ScrollArea>

                <div className="flex justify-end">
                    <Button onClick={() => onOpenChange(false)}>閉じる</Button>
                </div>
            </DialogContent>
        </Dialog>
    );
}

// 難易度カードコンポーネント
function DifficultyCard({
    level,
    emoji,
    strength,
    depth,
    time,
    features,
}: {
    level: string;
    emoji: string;
    strength: string;
    depth: string;
    time: string;
    features: string[];
}) {
    return (
        <div className="border rounded-lg p-4 space-y-2">
            <div className="flex items-center justify-between">
                <h4 className="font-semibold flex items-center gap-2">
                    <span className="text-2xl">{emoji}</span>
                    {level}
                </h4>
                <span className="text-sm text-muted-foreground">{strength}</span>
            </div>
            <div className="flex gap-4 text-sm text-muted-foreground">
                <span>読み: {depth}</span>
                <span>思考時間: {time}</span>
            </div>
            <ul className="text-sm space-y-1">
                {features.map((feature) => (
                    <li key={feature} className="flex items-start gap-1">
                        <span className="text-muted-foreground">•</span>
                        <span>{feature}</span>
                    </li>
                ))}
            </ul>
        </div>
    );
}

// 評価項目コンポーネント
function EvaluationItem({ title, description }: { title: string; description: string }) {
    return (
        <div className="flex flex-col gap-1">
            <h4 className="font-medium text-sm">{title}</h4>
            <p className="text-sm text-muted-foreground">{description}</p>
        </div>
    );
}

// FAQアイテムコンポーネント
function FAQItem({ question, answer }: { question: string; answer: string }) {
    return (
        <div className="space-y-1">
            <p className="font-medium text-sm">Q: {question}</p>
            <p className="text-sm text-muted-foreground">A: {answer}</p>
        </div>
    );
}
