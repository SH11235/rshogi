//! tools の結合テスト。
//!
//! テストファイルごとに実行ファイルを分けるとリンク回数が増えるため、1 つにまとめている。
//! テストを追加するときは、このディレクトリに module を置いてここへ登録する。

mod analyze_selfplay_validity;
mod bin_test_targets;
mod book_backprop_check;
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
mod spsa_plot_output;
mod spsa_run_dir_integration;
mod spsa_schedule_validation;
mod static_layer_stacks_load;
mod teacher_output_alias_safety;
mod tournament_adjudication_integration;
mod tournament_error_retry_integration;
mod tournament_output_identity;
mod tournament_shutdown_integration;
