//! 結合テストを 1 つのテストバイナリにまとめる。`tests/*.rs` はファイルごとに別バイナリになり、
//! バイナリごとに依存 crate 込みのリンクが走るため。結合テストはこのディレクトリにモジュールとして追加する。

mod d1_migrations_contract;
mod kifu_archive_import;
mod result_codes_contract;
mod wrangler_environment_toml_consistency;
mod wrangler_template_consistency;
