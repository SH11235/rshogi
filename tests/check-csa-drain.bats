#!/usr/bin/env bats
# scripts/check-csa-drain.sh の bats テスト。
# 各 test では `curl` を bash function で override し、固定的な response sequence を
# 返すことで polling 挙動を検証する。jq は実物を使う。

setup() {
    SCRIPT="${BATS_TEST_DIRNAME}/../scripts/check-csa-drain.sh"
    TMPDIR_TEST=$(mktemp -d)
    export FAKE_RESPONSE_FILE="$TMPDIR_TEST/responses"
    export FAKE_INDEX_FILE="$TMPDIR_TEST/index"
    echo 0 > "$FAKE_INDEX_FILE"
    # `curl` mock: FAKE_RESPONSE_FILE の N 行目 (1-indexed) を返す。`-w '\n%{http_code}'`
    # に倣い "<json>\n<status>" 形式で stdout 出力。最終行を超えたら最終行を反復。
    export PATH="$TMPDIR_TEST/bin:$PATH"
    mkdir -p "$TMPDIR_TEST/bin"
    cat > "$TMPDIR_TEST/bin/curl" <<'CURL_EOF'
#!/usr/bin/env bash
idx=$(cat "$FAKE_INDEX_FILE")
idx=$((idx + 1))
echo "$idx" > "$FAKE_INDEX_FILE"
total=$(wc -l < "$FAKE_RESPONSE_FILE")
if [ "$idx" -gt "$total" ]; then
    idx="$total"
fi
line=$(sed -n "${idx}p" "$FAKE_RESPONSE_FILE")
status="${line%%|*}"
body="${line#*|}"
# `--max-time` 等の引数は無視。`-w '\n%{http_code}'` 模倣で body + "\n" + status
printf '%s\n%s' "$body" "$status"
CURL_EOF
    chmod +x "$TMPDIR_TEST/bin/curl"
}

teardown() {
    rm -rf "$TMPDIR_TEST"
}

# 各行 = 1 fetch の応答 (`<status>|<json_body>` 形式)。
write_responses() {
    : > "$FAKE_RESPONSE_FILE"
    for line in "$@"; do
        printf '%s\n' "$line" >> "$FAKE_RESPONSE_FILE"
    done
}

# poll-interval を 0 / max-wait を大きく / retry を 0 で試験高速化。
run_drain() {
    run "$SCRIPT" \
        --live-url "https://example.test/api/v1/games/live" \
        --poll-interval-sec 0 \
        --max-wait-sec 60 \
        --retry-on-fetch-error 2 \
        --require-stable-zero "${REQUIRE_STABLE:-3}" \
        "$@"
}

# T1 happy path: 初回 0 件 → N=3 連続観測で drained
@test "T1 happy: zero from the start, drained after N consecutive observations" {
    write_responses \
        '200|{"live_games":[],"next_cursor":null}' \
        '200|{"live_games":[],"next_cursor":null}' \
        '200|{"live_games":[],"next_cursor":null}'
    REQUIRE_STABLE=3 run_drain
    [ "$status" -eq 0 ]
    [[ "$output" == *'"drained":true'* ]]
    [[ "$output" == *'"final_count":0'* ]]
}

# T2 eventual consistency flake: 0,1,0,0,0 で N=3 が成立
@test "T2 flake: zero -> one -> zero zero zero, drained" {
    write_responses \
        '200|{"live_games":[],"next_cursor":null}' \
        '200|{"live_games":[{"game_id":"g1"}],"next_cursor":null}' \
        '200|{"live_games":[],"next_cursor":null}' \
        '200|{"live_games":[],"next_cursor":null}' \
        '200|{"live_games":[],"next_cursor":null}'
    REQUIRE_STABLE=3 run_drain
    [ "$status" -eq 0 ]
    [[ "$output" == *'"drained":true'* ]]
}

# T3 timeout: count > 0 が継続して max-wait 超過
@test "T3 timeout: nonzero forever, exit 1" {
    write_responses \
        '200|{"live_games":[{"game_id":"g1"}],"next_cursor":null}'
    run "$SCRIPT" \
        --live-url "https://example.test/api/v1/games/live" \
        --poll-interval-sec 0 \
        --max-wait-sec 0 \
        --retry-on-fetch-error 1 \
        --require-stable-zero 3
    [ "$status" -eq 1 ]
    [[ "$output" == *'"drained":false'* ]]
}

# T4 fetch error 連続: retry 上限超えで exit 2
@test "T4 fetch error persistent: exit 2" {
    write_responses \
        '503|server error' \
        '503|server error' \
        '503|server error'
    run "$SCRIPT" \
        --live-url "https://example.test/api/v1/games/live" \
        --poll-interval-sec 0 \
        --max-wait-sec 60 \
        --retry-on-fetch-error 2 \
        --require-stable-zero 3
    [ "$status" -eq 2 ]
}

# T5 fetch error 一時的: retry 内で復活して drained
@test "T5 fetch error transient: recover and drain" {
    write_responses \
        '503|server error' \
        '200|{"live_games":[],"next_cursor":null}' \
        '200|{"live_games":[],"next_cursor":null}' \
        '200|{"live_games":[],"next_cursor":null}'
    REQUIRE_STABLE=3 run_drain --retry-on-fetch-error 3
    [ "$status" -eq 0 ]
    [[ "$output" == *'"drained":true'* ]]
}

# T6 pagination: page1 cursor=tok, page2 空 で count=page1+page2
@test "T6 pagination: cursor traversal accumulates count" {
    # 1 polling tick = 2 fetch (page1 + page2)。3 連続 0 で drain なので 3*2=6 fetch
    write_responses \
        '200|{"live_games":[],"next_cursor":"tok"}' \
        '200|{"live_games":[],"next_cursor":null}' \
        '200|{"live_games":[],"next_cursor":"tok"}' \
        '200|{"live_games":[],"next_cursor":null}' \
        '200|{"live_games":[],"next_cursor":"tok"}' \
        '200|{"live_games":[],"next_cursor":null}'
    REQUIRE_STABLE=3 run_drain
    [ "$status" -eq 0 ]
    [[ "$output" == *'"drained":true'* ]]
}

# T7 usage error: --live-url 欠落
@test "T7 usage: missing --live-url -> exit 3" {
    run "$SCRIPT" --max-wait-sec 10
    [ "$status" -eq 3 ]
    [[ "$output" == *"--live-url"* ]]
}

# T8 usage error: --live-url が query string を含む
@test "T8 usage: --live-url with query string -> exit 3" {
    run "$SCRIPT" --live-url "https://example.test/api/v1/games/live?limit=200"
    [ "$status" -eq 3 ]
    [[ "$output" == *"query string"* ]]
}

# T9 require-stable-zero=1 (境界): 1 回観測で即 drained
@test "T9 stable=1 boundary: drained after first zero" {
    write_responses \
        '200|{"live_games":[],"next_cursor":null}'
    REQUIRE_STABLE=1 run_drain
    [ "$status" -eq 0 ]
    [[ "$output" == *'"drained":true'* ]]
}

# T10 pagination + count > 0: page1 に live_games 1 件、N=3 観測されない
@test "T10 pagination with nonzero: stable counter does not advance" {
    write_responses \
        '200|{"live_games":[{"game_id":"g1"}],"next_cursor":"tok"}' \
        '200|{"live_games":[],"next_cursor":null}'
    run "$SCRIPT" \
        --live-url "https://example.test/api/v1/games/live" \
        --poll-interval-sec 0 \
        --max-wait-sec 0 \
        --retry-on-fetch-error 1 \
        --require-stable-zero 3
    [ "$status" -eq 1 ]
    [[ "$output" == *'"final_count":1'* ]]
}

# T11 schema anomaly: live_games フィールド欠落 → fetch error (exit 2)、drained 誤判定しない
@test "T11 schema anomaly: missing live_games field -> exit 2 (not silent drained)" {
    write_responses \
        '200|{"ok":true}' \
        '200|{"ok":true}' \
        '200|{"ok":true}'
    run "$SCRIPT" \
        --live-url "https://example.test/api/v1/games/live" \
        --poll-interval-sec 0 \
        --max-wait-sec 60 \
        --retry-on-fetch-error 2 \
        --require-stable-zero 3
    [ "$status" -eq 2 ]
    [[ "$output" != *'"drained":true'* ]]
}

# T12 schema anomaly: live_games が null
@test "T12 schema anomaly: live_games is null -> exit 2" {
    write_responses \
        '200|{"live_games":null,"next_cursor":null}' \
        '200|{"live_games":null,"next_cursor":null}' \
        '200|{"live_games":null,"next_cursor":null}'
    run "$SCRIPT" \
        --live-url "https://example.test/api/v1/games/live" \
        --poll-interval-sec 0 \
        --max-wait-sec 60 \
        --retry-on-fetch-error 2 \
        --require-stable-zero 3
    [ "$status" -eq 2 ]
}

# T13 schema anomaly: live_games が object
@test "T13 schema anomaly: live_games is object -> exit 2" {
    write_responses \
        '200|{"live_games":{},"next_cursor":null}' \
        '200|{"live_games":{},"next_cursor":null}' \
        '200|{"live_games":{},"next_cursor":null}'
    run "$SCRIPT" \
        --live-url "https://example.test/api/v1/games/live" \
        --poll-interval-sec 0 \
        --max-wait-sec 60 \
        --retry-on-fetch-error 2 \
        --require-stable-zero 3
    [ "$status" -eq 2 ]
}

# T14 schema anomaly: next_cursor が number 等の不正型
@test "T14 schema anomaly: next_cursor is integer -> exit 2" {
    write_responses \
        '200|{"live_games":[],"next_cursor":42}' \
        '200|{"live_games":[],"next_cursor":42}' \
        '200|{"live_games":[],"next_cursor":42}'
    run "$SCRIPT" \
        --live-url "https://example.test/api/v1/games/live" \
        --poll-interval-sec 0 \
        --max-wait-sec 60 \
        --retry-on-fetch-error 2 \
        --require-stable-zero 3
    [ "$status" -eq 2 ]
}

