//! 結合テストを 1 つのテストバイナリにまとめる。`tests/*.rs` はファイルごとに別バイナリになり、
//! バイナリごとに依存 crate 込みのリンクが走るため。結合テストはこのディレクトリにモジュールとして追加する。

mod book_flow;
mod build_rs_source_version;
mod go_mate;
mod ponder_nodes;
mod source_version;
mod usi_flow;
