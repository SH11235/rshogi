//! 結合テストを 1 つのテストバイナリにまとめる。`tests/*.rs` はファイルごとに別バイナリになり、
//! バイナリごとに依存 crate 込みのリンクが走るため。結合テストはこのディレクトリにモジュールとして追加する。
//! ただし global allocator・global recorder・評価関数の設定などプロセス全体の状態を書き換えるテストは、
//! `cargo test` で他のテストと同じプロセスに同居しないよう `tests/*.rs` に別バイナリとして置く。

mod book_flow;
mod build_rs_source_version;
mod go_mate;
mod ponder_nodes;
mod source_version;
mod usi_flow;
