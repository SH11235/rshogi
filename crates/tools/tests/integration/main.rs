//! 結合テストを 1 つのテストバイナリにまとめる。`tests/*.rs` はファイルごとに別バイナリになり、
//! バイナリごとに依存 crate 込みのリンクが走るため。結合テストはこのディレクトリにモジュールとして追加する。
//! ただし global allocator・global recorder・評価関数の設定などプロセス全体の状態を書き換えるテストは、
//! `cargo test` で他のテストと同じプロセスに同居しないよう `tests/*.rs` に別バイナリとして置く。

mod analyze_selfplay_validity;
mod book_backprop_check;
mod book_tree_serve;
mod game_history_integration;
mod gensfen_resume_integration;
mod gensfen_sidecar_integration;
mod gensfen_teacher_labels_integration;
mod net_spsa_finalize;
mod pack_to_psv_integration;
mod psv_scatter_by_mask_integration;
mod psv_to_hcpe3_integration;
mod relabel_psv_integration;
mod shuffle_psv_determinism;
mod sprt_negative_nelo_args;
mod spsa_params_format;
mod spsa_plot_output;
mod spsa_run_dir_integration;
mod spsa_schedule_validation;
mod static_layer_stacks_load;
mod teacher_output_alias_safety;
mod teacher_resource_output_safety;
mod tournament_adjudication_integration;
mod tournament_error_retry_integration;
mod tournament_output_identity;
mod tournament_shutdown_integration;
