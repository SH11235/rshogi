#!/usr/bin/env bash
# tracked file に、この開発環境固有の絶対パスが混入していないか検査する。
#
# 共有データ (教師 / NNUE / progress / 学習 checkpoint) は環境変数 $SHOGI_DATA 配下に
# 置き、外部 repo は /path/to/... の placeholder で書く。環境固有の絶対パスは別マシン
# /別ユーザでは解決できず、repo を移すと silent に壊れ、公開時に local 構成を漏らす。
#
# 検出対象は本環境で実際に使うルートに限定する (現状 /home/sh11235, /mnt/nvme1)。
# `/home/[^/]+` のような汎用パターンにすると、floodgate サーバの正当なパス定数
# (`/home/shogi-server/www/x/`) や doc の placeholder まで誤検出するため広げない。
# 新しいマシンのルートを使い始めたら下の pattern に追加する。
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

pattern='/home/sh11235/|/mnt/nvme1/'
hits=$(git grep -nIE "$pattern" -- . ':(exclude)scripts/check-tracked-abs-paths.sh' || true)

if [ -n "$hits" ]; then
  echo "ERROR: この環境固有の絶対パスが tracked file にあります:" >&2
  echo "$hits" >&2
  echo >&2
  echo "data/model/progress は \$SHOGI_DATA を、外部 repo は /path/to/... を使ってください。" >&2
  exit 1
fi
echo "OK: tracked file に環境固有の絶対パスなし"
