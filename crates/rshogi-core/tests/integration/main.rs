//! 結合テストを 1 つのテストバイナリにまとめる。`tests/*.rs` はファイルごとに別バイナリになり、
//! バイナリごとに依存 crate 込みのリンクが走るため。結合テストはこのディレクトリにモジュールとして追加する。

mod analysis_resume;
mod build_rs_checks;
mod build_rs_result_features;
mod public_pv_bench;
mod search_evaluator_regression;
