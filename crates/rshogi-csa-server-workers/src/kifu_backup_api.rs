//! R2 の版付きコピーと、管理者向けの欠損オブジェクト復元。

use serde::Serialize;
use sha2::{Digest, Sha256};
use worker::{
    AbortController, Bucket, Conditional, Date, Delay, Env, Error, Fetch, Method, Request,
    RequestInit, RequestRedirect, Response, Result,
};

use crate::config::ConfigKeys;
use crate::games_index::{
    KEY_PREFIX as INDEX_PREFIX, games_index_key, is_external_source, is_games_index_key_of,
};
use crate::games_search_index::{OwnedGamesIndexEntry, SEARCH_ROW_COLUMNS};
use crate::kifu_backup::{
    Checkpoint, LEASE_MS, MAX_CSA_BYTES, MAX_META_BYTES, MAX_SCAN_ITEMS, MAX_SNAPSHOT_BYTES,
    SNAPSHOT_PREFIX, STATE_KEY, Snapshot, Verification, date_prefix,
};
use crate::viewer_api::{is_admin_request, no_store_error};
use crate::x1_paths::kifu_by_id_object_key;

const SOURCE_PREFIX: &str = "kifu-by-id/";
const API_PREFIX: &str = "/api/v1/admin/kifu-backup/";
const ALERT_INTERVAL_MS: u64 = 60 * 60 * 1000;
const MAX_RESTORE_INDEX_ITEMS: u32 = 1000;
const RESTORE_PREFLIGHT_MS: u64 = 20_000;

fn failure(message: &str) -> Error {
    Error::RustError(message.into())
}

#[derive(Serialize)]
struct Outcome {
    enabled: bool,
    busy: bool,
    processed: u32,
    cycle_complete: bool,
    deadline_reached: bool,
    state: Checkpoint,
}

impl Outcome {
    fn new(enabled: bool, state: Checkpoint) -> Self {
        Self {
            enabled,
            busy: false,
            processed: 0,
            cycle_complete: false,
            deadline_reached: false,
            state,
        }
    }
}

async fn read_bytes(
    bucket: &Bucket,
    key: &str,
    limit: usize,
) -> Result<Option<(worker::Object, Vec<u8>)>> {
    let Some(object) = bucket.get(key).execute().await? else {
        return Ok(None);
    };
    if object.size() > limit as u64 {
        return Err(failure("backup object exceeds size limit"));
    }
    let bytes = object
        .body()
        .ok_or_else(|| failure("backup object body missing"))?
        .bytes()
        .await?;
    if bytes.len() > limit {
        return Err(failure("backup object exceeds size limit"));
    }
    Ok(Some((object, bytes)))
}

async fn load_state(bucket: &Bucket) -> Result<(Checkpoint, Option<String>)> {
    match read_bytes(bucket, STATE_KEY, MAX_META_BYTES).await? {
        Some((object, bytes)) => {
            let state: Checkpoint = serde_json::from_slice(&bytes)?;
            if state
                .after_key
                .as_ref()
                .is_some_and(|key| !key.starts_with(SOURCE_PREFIX) || key.len() > 1024)
            {
                return Err(failure("invalid backup checkpoint"));
            }
            Ok((state, Some(object.etag())))
        }
        None => Ok((Checkpoint::default(), None)),
    }
}

async fn save_state(bucket: &Bucket, state: &Checkpoint, etag: Option<&str>) -> Result<String> {
    let condition = match etag {
        Some(etag) => Conditional {
            etag_matches: Some(etag.into()),
            ..Default::default()
        },
        None => Conditional {
            etag_does_not_match: Some("*".into()),
            ..Default::default()
        },
    };
    bucket
        .put(STATE_KEY, serde_json::to_vec(state)?)
        .only_if(condition)
        .execute()
        .await?
        .map(|object| object.etag())
        .ok_or_else(|| failure("backup checkpoint changed concurrently"))
}

async fn create_identical(bucket: &Bucket, key: &str, bytes: &[u8], limit: usize) -> Result<()> {
    if let Some((_, existing)) = read_bytes(bucket, key, limit).await? {
        return if existing == bytes {
            Ok(())
        } else {
            Err(failure("backup/restore object conflict"))
        };
    }
    let put = bucket
        .put(key, bytes.to_vec())
        .sha256(Sha256::digest(bytes).to_vec())
        .only_if(Conditional {
            etag_does_not_match: Some("*".into()),
            ..Default::default()
        })
        .execute()
        .await;
    match put {
        Ok(Some(_)) => Ok(()),
        // 並行実行が同じ版を作成すると、lock 下では再 PUT 自体が失敗し得る。
        result => match read_bytes(bucket, key, limit).await? {
            Some((_, existing)) if existing == bytes => Ok(()),
            _ => Err(result.err().unwrap_or_else(|| failure("backup/restore object conflict"))),
        },
    }
}

async fn search_row_matches(env: &Env, entry: &OwnedGamesIndexEntry) -> Result<bool> {
    let db = env.d1(ConfigKeys::GAMES_SEARCH_DB_BINDING)?;
    let sql = format!(
        "SELECT {SEARCH_ROW_COLUMNS}, result_kind FROM games_search_index WHERE game_id = ?"
    );
    let actual = db
        .prepare(&sql)
        .bind(&[worker::wasm_bindgen::JsValue::from_str(&entry.game_id)])?
        .first::<serde_json::Value>(None)
        .await?;
    match actual {
        Some(actual) => Ok(actual == crate::kifu_backup::restore_search_row(entry)?),
        None => Ok(true),
    }
}

async fn check_restore_indexes(
    env: &Env,
    primary: &Bucket,
    entry: &OwnedGamesIndexEntry,
    deadline_ms: u64,
) -> Result<Option<&'static str>> {
    if !search_row_matches(env, entry).await? {
        return Ok(Some("Restore conflicts with existing D1 row"));
    }
    if Date::now().as_millis() >= deadline_ms {
        return Ok(Some("Restore preflight timed out; no writes performed"));
    }
    // 日付由来の key は ID から引けない。全体を確認できない場合は欠損と決めつけない。
    let page = primary
        .list()
        .prefix(INDEX_PREFIX)
        .limit(MAX_RESTORE_INDEX_ITEMS)
        .execute()
        .await?;
    let expected = games_index_key(entry.ended_at_ms, &entry.game_id)
        .map_err(|_| failure("invalid restore index key"))?;
    for object in page.objects() {
        let key = object.key();
        if is_games_index_key_of(&key, &entry.game_id) && key != expected {
            return Ok(Some("Restore conflicts with another index for the same game"));
        }
    }
    if page.truncated() {
        return Ok(Some(
            "Restore index check exceeds 1000 objects; reconcile indexes before restoring",
        ));
    }
    if Date::now().as_millis() >= deadline_ms {
        return Ok(Some("Restore preflight timed out; no writes performed"));
    }
    Ok(None)
}
async fn copy_game(primary: &Bucket, backup: &Bucket, meta_key: &str) -> Result<Verification> {
    let (meta_object, meta_bytes) = read_bytes(primary, meta_key, MAX_META_BYTES)
        .await?
        .ok_or_else(|| failure("source meta disappeared"))?;
    let meta_json =
        String::from_utf8(meta_bytes).map_err(|_| failure("source meta is not UTF-8"))?;
    let meta: serde_json::Value = serde_json::from_str(&meta_json)?;
    let game_id = meta
        .get("game_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| failure("source game_id missing"))?;
    if meta_key != crate::x1_paths::kifu_by_id_meta_key(game_id) {
        return Err(failure("source meta key mismatch"));
    }
    let csa_key = kifu_by_id_object_key(game_id);
    let (csa_object, csa_bytes) = read_bytes(primary, &csa_key, MAX_CSA_BYTES)
        .await?
        .ok_or_else(|| failure("source CSA missing; export retry may be pending"))?;
    let verification = if meta.get("csa_sha256").is_some() {
        Verification::Digest
    } else if meta.get("source").and_then(|v| v.as_str()).is_some_and(is_external_source) {
        Verification::UnverifiedLegacy
    } else {
        Verification::NativeLegacy
    };
    let snapshot = Snapshot {
        schema_version: 1,
        game_id: game_id.into(),
        meta_version: meta_object.version(),
        meta_uploaded_ms: meta_object.uploaded().as_millis(),
        csa_version: csa_object.version(),
        verification,
        csa: String::from_utf8(csa_bytes).map_err(|_| failure("source CSA is not UTF-8"))?,
        meta_json,
    };
    let entry = snapshot.validate().map_err(Error::RustError)?;
    if verification == Verification::NativeLegacy {
        let date_key = format!(
            "{}/{}.csa",
            crate::datetime::format_date_path(entry.started_at_ms),
            entry.game_id
        );
        let (_, date_bytes) = read_bytes(primary, &date_key, MAX_CSA_BYTES)
            .await?
            .ok_or_else(|| failure("legacy native date copy missing"))?;
        if date_bytes != snapshot.csa.as_bytes() {
            return Err(failure("legacy native CSA copies disagree"));
        }
    }
    for (key, version) in [
        (meta_key, &snapshot.meta_version),
        (csa_key.as_str(), &snapshot.csa_version),
    ] {
        if primary.head(key).await?.is_none_or(|object| object.version() != *version) {
            return Err(failure("source changed during backup; retry required"));
        }
    }
    let (key, bytes) = snapshot.encode().map_err(Error::RustError)?;
    create_identical(backup, &key, &bytes, MAX_SNAPSHOT_BYTES).await?;
    Ok(verification)
}

async fn alert(env: &Env, state: &mut Checkpoint) {
    let now = Date::now().as_millis();
    if state.last_error.is_none() && !state.lagging(now) && state.unverified_legacy == 0 {
        return;
    }
    let Some(url) = env
        .var(ConfigKeys::KIFU_BACKUP_ALERT_URL)
        .ok()
        .map(|v| v.to_string())
        .filter(|v| !v.is_empty())
    else {
        state.notification = "unconfigured".into();
        return;
    };
    if state.last_alert_ms != 0 && now.saturating_sub(state.last_alert_ms) < ALERT_INTERVAL_MS {
        return;
    }
    let sent = async {
        let parsed = worker::Url::parse(&url).map_err(|_| failure("invalid notification URL"))?;
        if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(failure("invalid notification URL"));
        }
        let message = serde_json::json!({"text": format!(
            "rshogi kifu backup needs attention: failed={}, lagging={}, unverified_legacy={}. Check authenticated backup status.",
            state.last_error.is_some(), state.lagging(now), state.unverified_legacy,
        )});
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_body(Some(message.to_string().into()))
            .with_redirect(RequestRedirect::Manual);
        let mut request = Request::new_with_init(&url, &init)?;
        request.headers_mut()?.set("Content-Type", "application/json")?;
        let controller = AbortController::default();
        let signal = controller.signal();
        let fetch = Fetch::Request(request);
        let send = std::pin::pin!(fetch.send_with_signal(&signal));
        let timeout = std::pin::pin!(Delay::from(std::time::Duration::from_secs(2)));
        let response = match futures_util::future::select(send, timeout).await {
            futures_util::future::Either::Left((response, _)) => response?,
            futures_util::future::Either::Right(_) => {
                controller.abort();
                return Err(failure("notification delivery timed out"));
            }
        };
        if !(200..300).contains(&response.status_code()) {
            return Err(failure("notification delivery failed"));
        }
        Ok::<(), Error>(())
    }.await;
    state.last_alert_ms = now;
    state.notification = if sent.is_ok() { "delivered" } else { "failed" }.into();
    if sent.is_err() {
        crate::structured_log!(event: "kifu_backup_notification_failed", component: "kifu_backup");
    }
}

async fn run(env: &Env, deadline_ms: u64) -> Result<Outcome> {
    let Ok(backup) = env.bucket(ConfigKeys::KIFU_BACKUP_BUCKET_BINDING) else {
        return Ok(Outcome::new(false, Checkpoint::default()));
    };
    let (mut state, etag) = load_state(&backup).await?;
    let now = Date::now().as_millis();
    if state.lease_until_ms > now {
        let mut outcome = Outcome::new(true, state);
        outcome.busy = true;
        return Ok(outcome);
    }
    state.lease_until_ms = now.saturating_add(LEASE_MS);
    state.last_run_ms = now;
    state.last_error = None;
    if state.cycle_started_ms == 0 {
        state.cycle_started_ms = now;
        state.verified = 0;
        state.unverified_legacy = 0;
    }
    let mut etag = save_state(&backup, &state, etag.as_deref()).await?;
    let mut outcome = Outcome::new(true, state);
    let work = async {
        let primary = env.bucket(ConfigKeys::KIFU_BUCKET_BINDING)?;
        if Date::now().as_millis() >= deadline_ms {
            outcome.deadline_reached = true;
            return Ok::<(), Error>(());
        }
        let mut list = primary.list().prefix(SOURCE_PREFIX).limit(MAX_SCAN_ITEMS);
        if let Some(key) = &outcome.state.after_key {
            list = list.start_after(key);
        }
        let page = list.execute().await?;
        for object in page.objects() {
            if Date::now().as_millis() >= deadline_ms {
                outcome.deadline_reached = true;
                break;
            }
            let key = object.key();
            if key.ends_with(".meta.json") {
                let verification = copy_game(&primary, &backup, &key).await?;
                match verification {
                    Verification::UnverifiedLegacy => outcome.state.unverified_legacy += 1,
                    _ => outcome.state.verified += 1,
                }
                outcome.processed += 1;
            }
            outcome.state.after_key = Some(key);
            if Date::now().as_millis() >= outcome.state.lease_until_ms {
                return Err(failure("backup lease expired"));
            }
            etag = save_state(&backup, &outcome.state, Some(&etag)).await?;
        }
        if !outcome.deadline_reached && !page.truncated() {
            outcome.state.after_key = None;
            outcome.state.completed_at_ms = Date::now().as_millis();
            outcome.state.cycle_started_ms = 0;
            outcome.cycle_complete = true;
        }
        Ok(())
    }
    .await;
    if let Err(error) = work {
        outcome.state.last_error = Some(error.to_string());
    }
    alert(env, &mut outcome.state).await;
    outcome.state.lease_until_ms = 0;
    save_state(&backup, &outcome.state, Some(&etag)).await?;
    Ok(outcome)
}

pub(crate) async fn scheduled(env: &Env, deadline_ms: u64) {
    match run(env, deadline_ms).await {
        Ok(outcome) => crate::structured_log!(
            event: "kifu_backup_progress", component: "kifu_backup",
            enabled: outcome.enabled, busy: outcome.busy, processed: outcome.processed,
            complete: outcome.cycle_complete, deadline_reached: outcome.deadline_reached,
            failed: outcome.state.last_error.is_some(), unverified_legacy: outcome.state.unverified_legacy,
        ),
        Err(_) => {
            crate::structured_log!(event: "kifu_backup_failed", component: "kifu_backup");
            let mut state = Checkpoint {
                last_error: Some("checkpoint/binding unavailable".into()),
                ..Checkpoint::default()
            };
            alert(env, &mut state).await;
        }
    }
}

fn json<T: Serialize>(value: &T) -> Result<Response> {
    let mut response = Response::from_json(value)?;
    response.headers_mut().set("Cache-Control", "no-store")?;
    Ok(response)
}

pub(crate) async fn try_handle(req: &Request, env: &Env) -> Result<Option<Response>> {
    let url = req.url()?;
    let Some(action) = url.path().strip_prefix(API_PREFIX) else {
        return Ok(None);
    };
    if !is_admin_request(req, env)? || req.headers().get("Origin")?.is_some() {
        return Ok(Some(no_store_error("Forbidden", 403)?));
    }
    if url.as_str().len() > 4096 {
        return Ok(Some(no_store_error("Request URL too long", 414)?));
    }
    let response = match handle(req, env, action).await {
        Ok(response) => response,
        Err(_) => {
            crate::structured_log!(event: "kifu_backup_admin_failed", component: "kifu_backup");
            no_store_error("Backup storage error; inspect backup status", 502)?
        }
    };
    Ok(Some(response))
}

async fn handle(req: &Request, env: &Env, action: &str) -> Result<Response> {
    if req.method() == Method::Post && action == "run" {
        let outcome = run(env, Date::now().as_millis().saturating_add(20_000)).await?;
        return json(&outcome);
    }
    let Ok(backup) = env.bucket(ConfigKeys::KIFU_BACKUP_BUCKET_BINDING) else {
        return if req.method() == Method::Get && action == "status" {
            json(&serde_json::json!({"enabled":false,"notification":"unconfigured"}))
        } else {
            no_store_error("Backup disabled", 503)
        };
    };
    if req.method() == Method::Get && action == "status" {
        let (state, _) = load_state(&backup).await?;
        return json(
            &serde_json::json!({"enabled":true,"lagging":state.lagging(Date::now().as_millis()),"state":state}),
        );
    }
    let url = req.url()?;
    let params: std::collections::BTreeMap<String, String> =
        url.query_pairs().into_owned().collect();
    if req.method() == Method::Get && action == "snapshots" {
        let Some(date) = params.get("date") else {
            return no_store_error("date is required", 400);
        };
        let prefix = match date_prefix(date) {
            Ok(prefix) => prefix,
            Err(error) => return no_store_error(error, 400),
        };
        let mut list = backup.list().prefix(&prefix).limit(100);
        if let Some(after) = params.get("after") {
            if !after.starts_with(&prefix) || after.len() > 1024 {
                return no_store_error("invalid after key", 400);
            }
            list = list.start_after(after);
        }
        let page = list.execute().await?;
        let keys: Vec<String> = page.objects().iter().map(|object| object.key()).collect();
        return json(&serde_json::json!({"keys":keys,"truncated":page.truncated()}));
    }
    if req.method() == Method::Post && action == "restore" {
        let deadline_ms = Date::now().as_millis().saturating_add(RESTORE_PREFLIGHT_MS);
        let Some(key) = params
            .get("key")
            .filter(|key| key.starts_with(SNAPSHOT_PREFIX) && key.len() <= 1024)
        else {
            return no_store_error("invalid snapshot key", 400);
        };
        let Some((_, bytes)) = read_bytes(&backup, key, MAX_SNAPSHOT_BYTES).await? else {
            return no_store_error("Snapshot not found", 404);
        };
        let snapshot = match Snapshot::decode(key, &bytes) {
            Ok(snapshot) => snapshot,
            Err(error) => return no_store_error(error, 409),
        };
        let targets = match snapshot.restore_targets() {
            Ok(targets) => targets,
            Err(error) => return no_store_error(error, 409),
        };
        let primary = env.bucket(ConfigKeys::KIFU_BUCKET_BINDING)?;
        for (target, expected) in &targets {
            if let Some((_, actual)) = read_bytes(&primary, target, MAX_CSA_BYTES).await?
                && actual != *expected
            {
                return no_store_error("Restore conflicts with existing object", 409);
            }
        }
        let entry = snapshot.entry().map_err(Error::RustError)?;
        if let Some(reason) = check_restore_indexes(env, &primary, &entry, deadline_ms).await? {
            return no_store_error(reason, 409);
        }
        let apply = match params.get("apply").map(String::as_str) {
            None | Some("false") => false,
            Some("true") => true,
            _ => return no_store_error("apply must be true or false", 400),
        };
        if apply {
            for (target, bytes) in &targets {
                create_identical(&primary, target, bytes, MAX_CSA_BYTES).await?;
            }
            for (target, expected) in &targets {
                if read_bytes(&primary, target, MAX_CSA_BYTES)
                    .await?
                    .is_none_or(|(_, actual)| actual != *expected)
                {
                    return no_store_error("Restore target changed before D1 update", 409);
                }
            }
            if !search_row_matches(env, &entry).await? {
                return no_store_error("Restore D1 row changed before update", 409);
            }
            crate::games_search_index::upsert_owned(env, &entry).await?;
        }
        return json(
            &serde_json::json!({"applied":apply,"game_id":snapshot.game_id,"targets":targets.iter().map(|(key,_)| key).collect::<Vec<_>>()}),
        );
    }
    no_store_error("Not Found", 404)
}
