//! NNUE アーキテクチャ定義用マクロ
//!
//! `define_l1_variants!` マクロで L1 enum を生成し、
//! 新しいアーキテクチャ追加時の作業量を最小化する。

/// L1 enum（第3階層）を定義するマクロ
///
/// L2/L3/活性化の組み合わせごとに enum バリアントを生成し、
/// 以下のメソッドを自動実装する:
/// - `evaluate()`: 評価値計算
/// - `refresh_accumulator()`: フル再計算
/// - `update_accumulator()`: 差分更新
/// - `forward_update_incremental()`: 前方差分更新
/// - `read()`: ファイル読み込みルーティング
/// - `architecture_name()`: アーキテクチャ名文字列
/// - `architecture_spec()`: アーキテクチャ仕様
/// - `SUPPORTED_SPECS`: サポートアーキテクチャ一覧
///
/// # 使用例
///
/// ```ignore
/// define_l1_variants!(
///     enum HalfKA_hm_L512,
///     feature_set HalfKA_hm,
///     l1 512,
///     acc AccumulatorHalfKA_hm<512>,
///     stack AccumulatorStackHalfKA_hm<512>,
///
///     variants {
///         (8,  96, CReLU,        "CReLU")    => CReLU_8_96     : HalfKA_hm512CReLU,
///         (8,  96, SCReLU,       "SCReLU")   => SCReLU_8_96    : HalfKA_hm512SCReLU,
///         (8,  96, PairwiseCReLU,"Pairwise") => Pairwise_8_96  : HalfKA_hm512Pairwise,
///     }
/// );
/// ```
#[macro_export]
macro_rules! define_l1_variants {
    (
        enum $Enum:ident,
        feature_set $FeatureSet:ident,
        l1 $L1:literal,
        acc $Acc:ty,
        stack $Stack:ty,

        variants {
            $(
                ($l2:literal, $l3:literal, $act:ident, $act_name:literal)
                    => $Var:ident : $Ty:ty
            ),+ $(,)?
        }
    ) => {
        /// L1 サイズ固定のネットワークバリアント
        ///
        /// L2/L3/活性化の組み合わせごとにバリアントを持つ。
        /// 新しい組み合わせ追加は、このマクロ定義に1行追加するだけ。
        pub enum $Enum {
            $(
                $Var(Box<$Ty>),
            )+
        }

        impl $Enum {
            /// 評価値を計算
            ///
            /// stack から現在の Accumulator を取得し、評価を行う。
            #[inline(always)]
            pub fn evaluate(&self, pos: &Position, stack: &$Stack) -> Value {
                let acc = stack.top();
                match self {
                    $(Self::$Var(net) => net.evaluate(pos, acc),)+
                }
            }

            /// Accumulator をフル再計算
            #[inline(always)]
            pub fn refresh_accumulator(&self, pos: &Position, stack: &mut $Stack) {
                let acc = stack.top_mut();
                match self {
                    $(Self::$Var(net) => net.refresh_accumulator(pos, acc),)+
                }
            }

            /// 差分更新（dirty piece ベース）
            #[inline(always)]
            pub fn update_accumulator(
                &self,
                pos: &Position,
                dirty: &DirtyPiece,
                stack: &mut $Stack,
                source_idx: usize,
            ) {
                let (acc, prev) = stack.top_and_source(source_idx);
                match self {
                    $(Self::$Var(net) => net.update_accumulator(pos, dirty, acc, prev),)+
                }
            }

            /// 前方差分更新を試みる（成功したら true）
            #[inline(always)]
            pub fn forward_update_incremental(
                &self,
                pos: &Position,
                stack: &mut $Stack,
                source_idx: usize,
            ) -> bool {
                match self {
                    $(Self::$Var(net) => net.forward_update_incremental(pos, stack, source_idx),)+
                }
            }

            /// ファイルから読み込み
            ///
            /// ヘッダー情報の L2/L3/活性化に基づいて適切なバリアントを選択。
            pub fn read<R: std::io::Read + std::io::Seek>(
                reader: &mut R,
                l2: usize,
                l3: usize,
                activation: Activation,
            ) -> std::io::Result<Self> {
                match (l2, l3, activation) {
                    $(
                        ($l2, $l3, Activation::$act) => {
                            let net = <$Ty>::read(reader)?;
                            Ok(Self::$Var(Box::new(net)))
                        }
                    )+
                    _ => Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "Unsupported {} L1={} architecture: L2={}, L3={}, activation={:?}",
                            stringify!($FeatureSet), $L1, l2, l3, activation
                        ),
                    )),
                }
            }

            /// アーキテクチャ名を取得
            pub fn architecture_name(&self) -> &'static str {
                match self {
                    $(
                        Self::$Var(_) => concat!(
                            stringify!($FeatureSet), "-",
                            stringify!($L1), "-",
                            stringify!($l2), "-",
                            stringify!($l3), "-",
                            $act_name
                        ),
                    )+
                }
            }

            /// アーキテクチャ仕様を取得
            pub fn architecture_spec(&self) -> ArchitectureSpec {
                match self {
                    $(
                        Self::$Var(_) => ArchitectureSpec {
                            feature_set: FeatureSet::$FeatureSet,
                            l1: $L1,
                            l2: $l2,
                            l3: $l3,
                            activation: Activation::$act,
                        },
                    )+
                }
            }

            /// サポートするアーキテクチャ一覧
            pub const SUPPORTED_SPECS: &'static [ArchitectureSpec] = &[
                $(
                    ArchitectureSpec {
                        feature_set: FeatureSet::$FeatureSet,
                        l1: $L1,
                        l2: $l2,
                        l3: $l3,
                        activation: Activation::$act,
                    },
                )+
            ];

            /// L1 サイズを取得
            #[inline]
            pub const fn l1_size(&self) -> usize {
                $L1
            }
        }
    };
}

// マクロをクレート内で使えるようにエクスポート
pub use define_l1_variants;

#[cfg(test)]
mod tests {
    // マクロのコンパイルテストは halfka/l*.rs で実施
}
