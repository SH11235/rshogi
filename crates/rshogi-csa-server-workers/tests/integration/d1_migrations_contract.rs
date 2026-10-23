//! viewer 検索 D1 migration の additive / 配線契約。

use std::fs;
use std::path::PathBuf;

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn deploy_workflow_applies_d1_migrations_before_each_worker_deploy() {
    let workflow = repo_file("../../.github/workflows/deploy-workers.yml");
    for env in ["staging", "production"] {
        let migration = format!(
            "wrangler d1 migrations apply GAMES_SEARCH_DB --remote --config wrangler.{env}.toml"
        );
        let deploy = format!("command: deploy --config wrangler.{env}.toml");
        let migration_pos = workflow
            .find(&migration)
            .unwrap_or_else(|| panic!("deploy workflow missing D1 migration command for {env}"));
        let deploy_pos = workflow
            .find(&deploy)
            .unwrap_or_else(|| panic!("deploy workflow missing worker deploy for {env}"));
        assert!(migration_pos < deploy_pos, "{env} migration must run before worker deploy");
    }
}

#[test]
fn secret_sync_validates_player_keyring_without_printing_value() {
    let workflow = repo_file("../../.github/workflows/secret-sync.yml");
    for required in [
        "pulumi env open \"${ESC_ENV}\" --format json",
        "required_keys=(ADMIN_API_TOKEN PLAYER_ID_SECRET)",
        "utf8bytelength >= 32",
        "length <= 8",
        "try fromjson catch null",
        "^[a-z0-9]{1,16}$",
    ] {
        assert!(workflow.contains(required), "secret strength gate missing {required}");
    }
    assert!(!workflow.contains("esc env open \"${ESC_ENV}\""));
    assert!(workflow.contains("値は非表示"));
}
