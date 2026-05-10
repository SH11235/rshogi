import type { WebSocket } from "miniflare";

/**
 * Miniflare 4 の `WebSocket` を行 (`\n` 区切り) 単位で読み取るためのバッファ。
 *
 * `lobby.test.ts` (issue #631 / #582) と `rate_limit.test.ts` (issue #622 PR3a)
 * の inline 重複ヘルパを共有化したもの (PR #699 claude[bot] review P2 follow-up)。
 *
 * **デフォルト 3000 ms timeout の根拠**: 共有化前の 2 inline 版がいずれも
 * 3000 ms を採用していた。本値で smoke が green である観測実績を保つために
 * 不変条件として固定する (`harness.ts::CsaClient.recvLine` の 5000 ms とは
 * 別の値域 — そちらは LOGIN handshake 経路用で本ヘルパとは責務が異なる)。
 *
 * **listener 二重登録の警告**: 本関数は 1 度の呼び出しごとに `message` /
 * `close` listener を 1 セット登録する。同 `ws` で複数 buffer を作ると同一
 * フレームが両 buffer の queue に積まれて test の正答性が壊れるため、同一 WS に
 * 対しては必ず 1 buffer のみを使う契約。
 */
export interface WebSocketLineBuffer {
  /**
   * 次の 1 行を取り出す。`\n` までのフレーム断片が揃ってない場合は新フレーム
   * 到着まで待ち、`timeoutMs` 経過で `Error("takeLine timeout after Xms")` で
   * reject。WS が close されると以降の呼び出しは即 `Error("connection closed")`
   * で reject。
   */
  takeLine(timeoutMs?: number): Promise<string>;
}

/**
 * `WebSocket` から行単位読み取り用の {@link WebSocketLineBuffer} を組み立てる。
 *
 * 使用例:
 * ```ts
 * const ws = await connectLobby(mf);
 * const buf = readLineFromWebSocket(ws);
 * ws.send("LOGIN_LOBBY alice+game-eval+black anything\n");
 * expect(await buf.takeLine()).toBe("LOGIN_LOBBY:alice OK");
 * ```
 */
export function readLineFromWebSocket(ws: WebSocket): WebSocketLineBuffer {
  let buffer = "";
  const queue: string[] = [];
  // queue (= 既受信フレーム) と waiters (= 受信前 takeLine 呼び出し) を分けて
  // 持つのは「フレーム → takeLine」「takeLine → フレーム」両方向の到着順序を
  // race-free に扱うため。フレーム到着時に waiters があれば最古を resolve、
  // なければ queue に積み、takeLine 呼び出し時に queue があれば即返却、
  // なければ自分を waiters に積む — どちらの順序でも 1 フレーム = 1 takeLine
  // の対応が崩れない。
  const waiters: Array<{
    resolve: (s: string) => void;
    reject: (e: Error) => void;
  }> = [];
  let closed = false;

  ws.addEventListener("message", (ev) => {
    const data =
      typeof ev.data === "string"
        ? ev.data
        : new TextDecoder().decode(ev.data as ArrayBuffer);
    buffer += data;
    while (true) {
      const idx = buffer.indexOf("\n");
      if (idx < 0) break;
      const line = buffer.slice(0, idx);
      buffer = buffer.slice(idx + 1);
      const w = waiters.shift();
      if (w) w.resolve(line);
      else queue.push(line);
    }
  });
  ws.addEventListener("close", () => {
    // close 後に未解決の waiters を放置すると test 側 Promise が未 settle で
    // hang する (vitest が timeout で気付くまで遅延が発生)。明示的に reject
    // して即時失敗に倒す。
    closed = true;
    while (waiters.length > 0) {
      const w = waiters.shift();
      w?.reject(new Error("connection closed"));
    }
  });

  return {
    takeLine(timeoutMs = 3000): Promise<string> {
      if (queue.length > 0) return Promise.resolve(queue.shift()!);
      if (closed) return Promise.reject(new Error("connection closed"));
      return new Promise<string>((resolve, reject) => {
        const entry = {
          resolve: (s: string) => {
            clearTimeout(timer);
            resolve(s);
          },
          reject: (e: Error) => {
            clearTimeout(timer);
            reject(e);
          },
        };
        // timeout 発火時に waiters から自分を抜く: 抜き忘れると後続フレームが
        // 自分の resolve を呼んで二重解決を起こす (Promise 仕様で no-op だが
        // queue 側の bookkeeping が腐る)。
        const timer = setTimeout(() => {
          const i = waiters.indexOf(entry);
          if (i >= 0) waiters.splice(i, 1);
          reject(new Error(`takeLine timeout after ${timeoutMs}ms`));
        }, timeoutMs);
        waiters.push(entry);
      });
    },
  };
}
