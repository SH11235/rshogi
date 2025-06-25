import { P2PTestPanel } from "../components/P2PTestPanel";

export function P2PTestPage() {
    return (
        <div className="min-h-screen bg-gray-100 py-8">
            <div className="container mx-auto px-4">
                <h1 className="text-3xl font-bold mb-4">Shogi P2P Test</h1>
                <P2PTestPanel />
            </div>
        </div>
    );
}
