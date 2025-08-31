# エンジン設定オプション リファレンス

## 概要

このドキュメントは、将棋エンジンの設定に使用可能なすべてのUSIプロトコルオプションとコマンドラインオプションを記載しています。

## コマンドラインオプション

### --debug / -d
- **説明**: デバッグログを有効化します。
- **デフォルト**: 無効
- **使用例**: `./engine-cli --debug`

### --allow-null-move
- **説明**: エラー時にnull move（0000）を返すことを許可します。
- **デフォルト**: 無効
- **使用例**: `./engine-cli --allow-null-move`

#### --allow-null-moveがデフォルトで無効な理由

このオプションはデフォルトで無効になっています。理由：

1. **USI仕様準拠**: null move（0000）はUSI仕様で定義されていない動作です。仕様に準拠するため、デフォルトでは`resign`を送信します。

2. **GUI互換性の問題**: 一部のGUIでは`0000`を受信すると、ponderタブやanalysisタブがクラッシュすることが報告されています。これは特に解析中に問題となることがあります。

3. **安全性優先**: 予期しない動作を避けるため、デフォルトでは最も安全な選択として仕様に準拠した`resign`を送信します。

#### いつ--allow-null-moveを使用すべきか

以下の場合にのみ、このオプションの使用を検討してください：

- 特定のGUIがnull moveを期待している場合
- null moveを適切に処理できることが確認されているGUIを使用している場合
- テスト環境でエンジンの動作を確認する場合

**注意**: プロダクション環境や大会での使用は推奨されません。

## USIプロトコルオプション

これらのオプションはUSIプロトコルの`setoption`コマンドを使用して設定できます。

### 1. USI_Hash
- **タイプ**: spin (数値)
- **デフォルト**: 16
- **範囲**: 1-1024
- **単位**: MB
- **説明**: ハッシュテーブル（置換表）のサイズをメガバイト単位で指定します。値が大きいほど多くの局面を記憶でき、探索効率が向上します。
- **例**: `setoption name USI_Hash value 256`

### 2. Threads
- **タイプ**: spin (数値)
- **デフォルト**: 1
- **範囲**: 1-256
- **説明**: 並列探索に使用するCPUスレッド数。マルチコアシステムでは、スレッド数を増やすことで解析速度が向上します。
- **例**: `setoption name Threads value 4`

### 3. USI_Ponder
- **タイプ**: check (真偽値)
- **デフォルト**: true
- **説明**: 先読み（相手の手番中の思考）を有効にします。有効にすると、エンジンは相手の手番中も思考を続けます。
- **例**: `setoption name USI_Ponder value false`

### 4. EngineType
- **タイプ**: combo (選択式)
- **デフォルト**: Material
- **値**: Material, Nnue, Enhanced, EnhancedNnue
- **説明**: エンジンタイプを選択します。探索アルゴリズムと評価関数の両方を決定します。
  - **EnhancedNnue** (推奨): 高度な探索技術 + NNUE評価関数
  - **Nnue**: 基本探索 + NNUE評価関数
  - **Enhanced**: 高度な探索技術 + 駒価値評価
  - **Material**: 基本探索 + 駒価値評価
- **例**: `setoption name EngineType value EnhancedNnue`

### 5. ByoyomiPeriods / USI_ByoyomiPeriods
- **タイプ**: spin (数値)
- **デフォルト**: 1 (または "default" でエンジンのデフォルト値)
- **範囲**: 1-10
- **説明**: 時間制御における秒読み回数。互換性のため、両方のオプション名がサポートされています。
- **例**: `setoption name ByoyomiPeriods value 3`
- **特殊値**: `setoption name ByoyomiPeriods value default` (エンジンのデフォルト値を使用)

### 6. ByoyomiEarlyFinishRatio
- **タイプ**: spin (数値)
- **デフォルト**: 80
- **範囲**: 50-95
- **単位**: パーセント
- **説明**: 探索を終了する前に使用する秒読み時間の割合。安全マージンを残すためにエンジンが思考を停止するタイミングを制御します。
- **例**: `setoption name ByoyomiEarlyFinishRatio value 85`

### 7. PVStabilityBase
- **タイプ**: spin (数値)
- **デフォルト**: 80
- **範囲**: 10-200
- **単位**: ミリ秒
- **説明**: 主要変化（PV）の安定性チェックのための基準時間閾値。最善手がまだ変化している場合、エンジンは探索を継続する可能性があります。
- **例**: `setoption name PVStabilityBase value 100`

### 8. PVStabilitySlope
- **タイプ**: spin (数値)
- **デフォルト**: 5
- **範囲**: 0-20
- **単位**: 深さ当たりのミリ秒
- **説明**: PV安定性のための探索深さ当たりの追加時間。深い探索ほど安定化のための時間が長くなります。
- **例**: `setoption name PVStabilitySlope value 8`

### 9. OverheadMs / ByoyomiOverheadMs / ByoyomiSafetyMs
- **タイプ**: spin (数値)
- **デフォルト**: OverheadMs=50, ByoyomiOverheadMs=1000, ByoyomiSafetyMs=500
- **単位**: ミリ秒
- **説明**:
  - `OverheadMs`: 通信/GUIの平均遅延（丸め停止時に控除）
  - `ByoyomiOverheadMs`: 秒読み時の最悪遅延（ハード上限の安全側に反映）
  - `ByoyomiSafetyMs`: 秒読みハード上限の追加安全マージン

### 10. SlowMover
- **タイプ**: spin (数値)
- **デフォルト**: 100
- **範囲**: 50-200（%）
- **説明**: 最適時間（soft）を倍率で増減（100=1.0x）。序盤を厚く・終盤を薄くなどの大まかな配分調整に使用します。
- **例**: `setoption name SlowMover value 120`

### 11. MaxTimeRatioPct
- **タイプ**: spin (数値)
- **デフォルト**: 500
- **範囲**: 100-800（%=1.00-8.00倍）
- **説明**: `hard <= soft * (pct/100)` の上限を設定。極端に長い思考を抑制する安全弁です。
- **例**: `setoption name MaxTimeRatioPct value 300`（3.00倍）

### 12. MoveHorizonTriggerMs / MoveHorizonMinMoves
- **タイプ**: spin (数値)
- **デフォルト**: 0（無効）/ 0（無効）
- **範囲**: Trigger=0-600000(ms), MinMoves=0-200
- **説明**: Sudden-death（Fischerでinc=0）時の切れ負けガード。
  - `remain <= Trigger` で発動し、`hard <= remain / MinMoves` に抑制します。
  - 小さな値から試し、感触を見ながら調整してください。

## 使用例

### 大会用の設定:
```
setoption name EngineType value EnhancedNnue
setoption name USI_Hash value 256
setoption name Threads value 4
setoption name USI_Ponder value true
setoption name ByoyomiEarlyFinishRatio value 85
```

### 解析用の設定:
```
setoption name EngineType value EnhancedNnue
setoption name USI_Hash value 512
setoption name Threads value 8
setoption name USI_Ponder value false
```

### 低メモリ環境用の設定:
```
setoption name EngineType value Enhanced
setoption name USI_Hash value 16
setoption name Threads value 1
```

### テスト/デバッグ用の設定:
```
setoption name EngineType value Material
setoption name USI_Hash value 16
setoption name Threads value 1
setoption name USI_Ponder value false
```

## 注意事項

1. オプションが有効になるためには、`isready`コマンドの前に設定する必要があります。
2. エンジンはオプション値を検証し、無効な設定を拒否します。
3. 一部のオプション（EngineTypeなど）は、エンジンの動作とパフォーマンスに大きな影響を与える可能性があります。
4. エンジンタイプの詳細については、`docs/engine-types-guide.md`を参照してください。

## 時間管理オプションの影響

時間管理オプション（ByoyomiEarlyFinishRatio、PVStabilityBase、PVStabilitySlope）は連携して、エンジンの時間管理方法を制御します：

- **ByoyomiEarlyFinishRatio**: 早めに終了することで時間切れを防ぎます
- **PVStabilityBase + PVStabilitySlope**: 最善手が不確定な場合に追加の思考時間を許可します

これらのパラメータは、正確な時間管理が重要な秒読み時間制御において特に重要です。

## 追加の使用例（時間ポリシー）

### より長めに考える（解析向き）
```
setoption name SlowMover value 120
setoption name MaxTimeRatioPct value 300
```

### 切れ負け対策（サドンデス想定）
```
setoption name MoveHorizonTriggerMs value 30000
setoption name MoveHorizonMinMoves value 15
```

### ストキャスティック・ポンダー（試験的）
```
setoption name Stochastic_Ponder value true
```
- **説明**: `ponderhit` 時に、通常探索へ再始動する前提の特別動作を有効化します。
- 備考: 本実装では `ponderhit` 受領時に時間起点と配分を再初期化して継続（再始動同等）します。
