interface Env {
    ASSETS: Fetcher;
}

export default {
    async fetch(request: Request, env: Env): Promise<Response> {
        // 静的アセットを取得
        const response = await env.ASSETS.fetch(request);

        // レスポンスヘッダーを追加（WASM SharedArrayBuffer対応）
        const newHeaders = new Headers(response.headers);
        newHeaders.set("Cross-Origin-Opener-Policy", "same-origin");
        newHeaders.set("Cross-Origin-Embedder-Policy", "require-corp");

        return new Response(response.body, {
            status: response.status,
            statusText: response.statusText,
            headers: newHeaders,
        });
    },
};
