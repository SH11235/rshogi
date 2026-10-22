//! 棋譜の版付きバックアップ形式と復元先の検証。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::datetime::format_date_path;
use crate::floodgate_history::validate_key_component;
use crate::games_index::{games_index_key, is_external_source};
use crate::games_search_index::OwnedGamesIndexEntry;
use crate::x1_paths::{kifu_by_id_meta_key, kifu_by_id_object_key};

pub(crate) const SNAPSHOT_PREFIX: &str = "snapshots/v1/";
#[cfg(target_arch = "wasm32")]
pub(crate) const STATE_KEY: &str = "control/kifu-backup-v1.json";
pub(crate) const MAX_CSA_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_META_BYTES: usize = 64 * 1024;
pub(crate) const MAX_SNAPSHOT_BYTES: usize = 6 * (MAX_CSA_BYTES + MAX_META_BYTES) + 4096;
#[cfg(target_arch = "wasm32")]
pub(crate) const MAX_SCAN_ITEMS: u32 = 128;
pub(crate) const RPO_TARGET_MS: u64 = 24 * 60 * 60 * 1000;
#[cfg(target_arch = "wasm32")]
pub(crate) const LEASE_MS: u64 = 60_000;

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn meta_with_csa_digest<T: Serialize>(
    meta: &T,
    csa: &[u8],
) -> Result<Vec<u8>, serde_json::Error> {
    let mut value = serde_json::to_value(meta)?;
    let object = value.as_object_mut().ok_or_else(|| {
        <serde_json::Error as serde::ser::Error>::custom("backup metadata must be an object")
    })?;
    object.insert("csa_sha256".into(), serde_json::Value::String(sha256_hex(csa)));
    serde_json::to_vec(&value)
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Verification {
    Digest,
    NativeLegacy,
    UnverifiedLegacy,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    pub schema_version: u32,
    pub game_id: String,
    pub meta_version: String,
    pub meta_uploaded_ms: u64,
    pub csa_version: String,
    pub verification: Verification,
    #[serde(with = "csa_bytes")]
    pub csa: Vec<u8>,
    pub meta_json: String,
}

mod csa_bytes {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

    use super::MAX_CSA_BYTES;

    #[derive(Deserialize, Serialize)]
    #[serde(untagged, deny_unknown_fields)]
    enum WireCsa {
        Utf8(String),
        Base64 { base64: String },
    }

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        // UTF-8 の既存表現を維持し、その他の byte 列も置換せず保存する。
        match std::str::from_utf8(bytes) {
            Ok(text) => serializer.serialize_str(text),
            Err(_) => WireCsa::Base64 {
                base64: STANDARD.encode(bytes),
            }
            .serialize(serializer),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let bytes = match WireCsa::deserialize(deserializer)? {
            WireCsa::Utf8(text) if text.len() <= MAX_CSA_BYTES => text.into_bytes(),
            WireCsa::Base64 { base64 } if base64.len() <= MAX_CSA_BYTES.div_ceil(3) * 4 => {
                STANDARD.decode(base64).map_err(D::Error::custom)?
            }
            _ => return Err(D::Error::custom("CSA exceeds size limit")),
        };
        if bytes.len() > MAX_CSA_BYTES {
            return Err(D::Error::custom("CSA exceeds size limit"));
        }
        Ok(bytes)
    }
}

impl Snapshot {
    pub(crate) fn entry(&self) -> Result<OwnedGamesIndexEntry, String> {
        let entry: OwnedGamesIndexEntry =
            serde_json::from_str(&self.meta_json).map_err(|_| "invalid backup meta")?;
        validate_key_component(&entry.game_id).map_err(|_| "invalid game_id")?;
        if entry.game_id != self.game_id || entry.game_id.len() > 256 {
            return Err("backup game_id mismatch".into());
        }
        if entry.started_at_ms > entry.ended_at_ms || entry.ended_at_ms > 253_402_300_799_999 {
            return Err("invalid backup timestamp".into());
        }
        if !matches!(entry.source.as_str(), "kifu" | "floodgate" | "wdoor" | "upload") {
            return Err("unsupported backup source".into());
        }
        Ok(entry)
    }

    pub(crate) fn validate(&self) -> Result<OwnedGamesIndexEntry, String> {
        if self.schema_version != 1
            || self.csa.is_empty()
            || self.csa.len() > MAX_CSA_BYTES
            || self.meta_json.len() > MAX_META_BYTES
        {
            return Err("unsupported backup format or size".into());
        }
        let entry = self.entry()?;
        let value: serde_json::Value =
            serde_json::from_str(&self.meta_json).map_err(|_| "invalid backup meta")?;
        match (value.get("csa_sha256"), self.verification) {
            (Some(digest), Verification::Digest)
                if digest.as_str() == Some(sha256_hex(&self.csa).as_str()) => {}
            (None, Verification::NativeLegacy) if !is_external_source(&entry.source) => {}
            (None, Verification::UnverifiedLegacy) if is_external_source(&entry.source) => {}
            _ => return Err("CSA/meta digest mismatch".into()),
        }
        Ok(entry)
    }

    pub(crate) fn encode(&self) -> Result<(String, Vec<u8>), String> {
        let entry = self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| "cannot encode snapshot")?;
        Ok((self.object_key(entry.started_at_ms, &bytes), bytes))
    }

    fn object_key(&self, started_at_ms: u64, bytes: &[u8]) -> String {
        format!(
            "{SNAPSHOT_PREFIX}{}/{}/{:016}-{}.json",
            format_date_path(started_at_ms),
            self.game_id,
            self.meta_uploaded_ms,
            sha256_hex(bytes),
        )
    }

    pub(crate) fn decode(key: &str, bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            return Err("snapshot too large".into());
        }
        let snapshot: Self = serde_json::from_slice(bytes).map_err(|_| "invalid snapshot JSON")?;
        let entry = snapshot.validate()?;
        let expected = snapshot.object_key(entry.started_at_ms, bytes);
        if key != expected {
            return Err("snapshot key/checksum mismatch".into());
        }
        Ok(snapshot)
    }

    pub(crate) fn restore_targets(&self) -> Result<Vec<(String, Vec<u8>)>, String> {
        let entry = self.validate()?;
        if self.verification == Verification::UnverifiedLegacy {
            return Err("legacy external snapshot requires source verification/re-import".into());
        }
        let mut targets = Vec::with_capacity(4);
        if !is_external_source(&entry.source) {
            targets.push((
                format!("{}/{}.csa", format_date_path(entry.started_at_ms), entry.game_id),
                self.csa.clone(),
            ));
        }
        targets.push((kifu_by_id_object_key(&entry.game_id), self.csa.clone()));
        targets.push((kifu_by_id_meta_key(&entry.game_id), self.meta_json.as_bytes().to_vec()));
        targets.push((
            games_index_key(entry.ended_at_ms, &entry.game_id).map_err(|_| "invalid index key")?,
            self.meta_json.as_bytes().to_vec(),
        ));
        Ok(targets)
    }
}

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn restore_search_row(
    entry: &OwnedGamesIndexEntry,
) -> Result<serde_json::Value, serde_json::Error> {
    let player_id = |id: &Option<String>, handle: &str| {
        if is_external_source(&entry.source) {
            None
        } else {
            Some(id.clone().unwrap_or_else(|| crate::player_identity::legacy_player_id(handle)))
        }
    };
    Ok(serde_json::json!({
        "game_id": entry.game_id,
        "started_at_ms": entry.started_at_ms,
        "ended_at_ms": entry.ended_at_ms,
        "sente_name": entry.black_handle,
        "gote_name": entry.white_handle,
        "black_player_id": player_id(&entry.black_player_id, &entry.black_handle),
        "white_player_id": player_id(&entry.white_player_id, &entry.white_handle),
        "wire_result_kind": entry.result_kind,
        "result_kind": crate::games_search_index::result_kind_for_search(&entry.end_reason),
        "end_reason": entry.end_reason,
        "moves_count": entry.moves_count,
        "clock_json": crate::games_search_index::canonical_clock_json(&entry.clock)?,
        "source": entry.source,
        "event": entry.event,
        "origin_url": entry.origin_url,
    }))
}
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Checkpoint {
    pub after_key: Option<String>,
    pub cycle_started_ms: u64,
    pub completed_at_ms: u64,
    pub last_run_ms: u64,
    pub lease_until_ms: u64,
    pub verified: u64,
    pub unverified_legacy: u64,
    pub last_error: Option<String>,
    pub last_alert_ms: u64,
    #[serde(default)]
    pub last_failure_alert_ms: u64,
    #[serde(default)]
    pub last_lag_alert_ms: u64,
    #[serde(default)]
    pub last_legacy_alert_ms: u64,
    pub notification: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DueAlerts {
    pub failure: bool,
    pub lag: bool,
    pub legacy: bool,
}

impl DueAlerts {
    pub fn any(self) -> bool {
        self.failure || self.lag || self.legacy
    }
}

impl Checkpoint {
    pub fn due_alerts(&self, now: u64) -> DueAlerts {
        let due = |last, interval| last == 0 || now.saturating_sub(last) >= interval;
        DueAlerts {
            failure: self.last_error.is_some() && due(self.last_failure_alert_ms, 60 * 60 * 1000),
            lag: self.lagging(now) && due(self.last_lag_alert_ms, 60 * 60 * 1000),
            legacy: self.unverified_legacy != 0
                && due(self.last_legacy_alert_ms, 24 * 60 * 60 * 1000),
        }
    }

    pub fn record_alert_attempt(&mut self, due: DueAlerts, now: u64) {
        if due.failure {
            self.last_failure_alert_ms = now;
        }
        if due.lag {
            self.last_lag_alert_ms = now;
        }
        if due.legacy {
            self.last_legacy_alert_ms = now;
        }
        self.last_alert_ms = now;
    }

    pub(crate) fn lagging(&self, now_ms: u64) -> bool {
        let reference = if self.completed_at_ms == 0 {
            self.cycle_started_ms
        } else {
            self.completed_at_ms
        };
        reference != 0 && now_ms.saturating_sub(reference) > RPO_TARGET_MS
    }
}

pub(crate) fn date_prefix(date: &str) -> Result<String, String> {
    let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|_| "date must be YYYY-MM-DD")?;
    if date != parsed.format("%Y-%m-%d").to_string() {
        return Err("date must be YYYY-MM-DD".into());
    }
    Ok(format!("{SNAPSHOT_PREFIX}{}/", parsed.format("%Y/%m/%d")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(source: &str, digest: bool) -> Snapshot {
        let csa = "V2.2\n%TORYO\n";
        let meta = serde_json::json!({
            "game_id":"test-1", "started_at_ms": 0, "ended_at_ms": 1000,
            "black_handle":"alice", "white_handle":"bob", "moves_count":0,
            "result_kind":"white_win", "end_reason":"resign", "source":source,
        });
        let meta_json = if digest {
            String::from_utf8(meta_with_csa_digest(&meta, csa.as_bytes()).unwrap()).unwrap()
        } else {
            meta.to_string()
        };
        Snapshot {
            schema_version: 1,
            game_id: "test-1".into(),
            meta_version: "meta-v1".into(),
            meta_uploaded_ms: 2000,
            csa_version: "csa-v1".into(),
            verification: if digest {
                Verification::Digest
            } else if is_external_source(source) {
                Verification::UnverifiedLegacy
            } else {
                Verification::NativeLegacy
            },
            csa: csa.as_bytes().to_vec(),
            meta_json,
        }
    }

    #[test]
    fn restore_search_row_matches_native_and_external_upsert_values() {
        let mut entry = snapshot("kifu", true).entry().unwrap();
        entry.end_reason = "RESIGN".into();
        entry.clock = Some(serde_json::json!({"total_sec": 300, "kind": "fischer"}));
        let row = restore_search_row(&entry).unwrap();
        assert_eq!(row.as_object().unwrap().len(), 15);
        assert_eq!(row["black_player_id"], crate::player_identity::legacy_player_id("alice"));
        assert_eq!(row["white_player_id"], crate::player_identity::legacy_player_id("bob"));
        assert_eq!(row["clock_json"], "{\"kind\":\"fischer\",\"total_sec\":300}");
        assert_eq!(row["result_kind"], "resignation");
        entry.black_player_id = Some("player-a".into());
        entry.event = Some("event-a".into());
        entry.origin_url = Some("https://example.com/kifu".into());
        let row = restore_search_row(&entry).unwrap();
        assert_eq!(row["black_player_id"], "player-a");
        assert_eq!(row["event"], "event-a");
        assert_eq!(row["origin_url"], "https://example.com/kifu");
        entry.source = "upload".into();
        let row = restore_search_row(&entry).unwrap();
        assert!(row["black_player_id"].is_null());
        assert!(row["white_player_id"].is_null());
    }
    #[test]
    fn snapshot_round_trip_and_tampering() {
        let original = snapshot("kifu", true);
        let (key, bytes) = original.encode().unwrap();
        assert!(key.starts_with("snapshots/v1/1970/01/01/test-1/"));
        let restored = Snapshot::decode(&key, &bytes).unwrap();
        assert_eq!(restored.csa, original.csa);
        assert!(Snapshot::decode("control/overwrite", &bytes).is_err());
        let mut changed = restored;
        changed.csa.push(b'x');
        assert!(changed.encode().is_err());
    }

    #[test]
    fn utf8_snapshots_keep_the_v1_string_representation() {
        let original = snapshot("kifu", true);
        let (key, bytes) = original.encode().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["csa"], "V2.2\n%TORYO\n");
        assert_eq!(Snapshot::decode(&key, &bytes).unwrap().csa, original.csa);
    }

    #[test]
    fn non_utf8_snapshots_restore_the_original_bytes_at_the_size_limit() {
        let mut original = snapshot("wdoor", true);
        original.csa = vec![0xff; MAX_CSA_BYTES];
        let meta: serde_json::Value = serde_json::from_str(&original.meta_json).unwrap();
        original.meta_json =
            String::from_utf8(meta_with_csa_digest(&meta, &original.csa).unwrap()).unwrap();
        let (key, bytes) = original.encode().unwrap();
        assert!(bytes.len() < MAX_SNAPSHOT_BYTES);
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["csa"]["base64"].is_string());
        let decoded = Snapshot::decode(&key, &bytes).unwrap();
        assert_eq!(decoded.csa, original.csa);
        assert_eq!(decoded.restore_targets().unwrap()[0].1, original.csa);
    }

    #[test]
    fn base64_payloads_reject_noncanonical_ambiguous_and_oversized_data() {
        use base64::{Engine, engine::general_purpose::STANDARD};

        let mut original = snapshot("wdoor", true);
        original.csa = vec![0xff];
        let meta: serde_json::Value = serde_json::from_str(&original.meta_json).unwrap();
        original.meta_json =
            String::from_utf8(meta_with_csa_digest(&meta, &original.csa).unwrap()).unwrap();
        let mut json = serde_json::to_value(&original).unwrap();
        for csa in [
            serde_json::json!({"base64":"/w"}),
            serde_json::json!({"base64":"/x=="}),
            serde_json::json!({"base64":"/w==\n"}),
            serde_json::json!({"base64":"_w=="}),
            serde_json::json!({"base64":"/w==", "text":"ambiguous"}),
            serde_json::json!({"base64":255}),
            serde_json::json!([255]),
            serde_json::json!({"base64":STANDARD.encode(vec![0xff; MAX_CSA_BYTES + 1])}),
            serde_json::json!({"base64":"A".repeat(MAX_CSA_BYTES.div_ceil(3) * 4 + 4)}),
            serde_json::json!("x".repeat(MAX_CSA_BYTES + 1)),
        ] {
            json["csa"] = csa;
            assert!(serde_json::from_value::<Snapshot>(json.clone()).is_err());
        }
        let duplicate = serde_json::to_string(&original)
            .unwrap()
            .replace("\"base64\":\"/w==\"", "\"base64\":\"/w==\",\"base64\":\"/w==\"");
        assert!(serde_json::from_str::<Snapshot>(&duplicate).is_err());
    }

    #[test]
    fn restore_uses_one_meta_version_for_both_indexes() {
        let original = snapshot("floodgate", true);
        let targets = original.restore_targets().unwrap();
        assert_eq!(targets.len(), 4);
        assert_eq!(targets[0].1, targets[1].1);
        assert_eq!(targets[2].1, targets[3].1);
        assert!(targets[2].0.starts_with("kifu-by-id/"));
        assert!(targets[3].0.starts_with("games-index/"));
        assert!(!targets.iter().any(|(key, _)| key.starts_with("buoys/")));
    }

    #[test]
    fn checksum_uses_stored_bytes_without_requiring_serializer_field_order() {
        let original = snapshot("kifu", true);
        let (key, bytes) = original.encode().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let pretty = serde_json::to_vec_pretty(&json).unwrap();
        assert_ne!(bytes, pretty);
        assert!(Snapshot::decode(&key, &pretty).is_err());
        let key = original.object_key(0, &pretty);
        assert!(Snapshot::decode(&key, &pretty).is_ok());
    }

    #[test]
    fn legacy_external_is_preserved_but_not_automatically_restored() {
        let legacy = snapshot("wdoor", false);
        assert!(legacy.encode().is_ok());
        assert!(legacy.restore_targets().is_err());
        assert_eq!(snapshot("wdoor", true).restore_targets().unwrap().len(), 3);
        assert_eq!(snapshot("kifu", false).restore_targets().unwrap().len(), 4);
    }

    #[test]
    fn invalid_keys_dates_and_digest_types_are_rejected() {
        let mut original = snapshot("kifu", true);
        original.game_id = "../../buoys/x".into();
        assert!(original.encode().is_err());
        for date in ["../control", "2026-02-30", "2026-1-01", "2026/01/01"] {
            assert!(date_prefix(date).is_err());
        }
        assert_eq!(date_prefix("2026-10-22").unwrap(), "snapshots/v1/2026/10/22/");
        let mut original = snapshot("kifu", true);
        original.meta_json =
            original.meta_json.replace("\"csa_sha256\":\"", "\"csa_sha256\":\"bad");
        assert!(original.validate().is_err());
    }

    #[test]
    fn lag_uses_last_completed_cycle_or_bootstrap_start() {
        let mut state = Checkpoint {
            cycle_started_ms: 1,
            ..Checkpoint::default()
        };
        assert!(!state.lagging(RPO_TARGET_MS));
        assert!(state.lagging(RPO_TARGET_MS + 2));
        state.completed_at_ms = RPO_TARGET_MS;
        assert!(!state.lagging(RPO_TARGET_MS + 2));
    }

    #[test]
    fn checkpoint_alerts_have_independent_cooldowns() {
        let now = 3 * RPO_TARGET_MS;
        let mut state = Checkpoint {
            unverified_legacy: 1,
            completed_at_ms: now,
            ..Checkpoint::default()
        };
        let legacy = state.due_alerts(now);
        assert_eq!(
            legacy,
            DueAlerts {
                legacy: true,
                ..DueAlerts::default()
            }
        );
        state.record_alert_attempt(legacy, now);
        state.last_error = Some("copy failed".into());
        let failure = state.due_alerts(now + 1);
        assert_eq!(
            failure,
            DueAlerts {
                failure: true,
                ..DueAlerts::default()
            }
        );
        state.record_alert_attempt(failure, now + 1);
        state.completed_at_ms = now - RPO_TARGET_MS - 1;
        let lag = state.due_alerts(now + 2);
        assert_eq!(
            lag,
            DueAlerts {
                lag: true,
                ..DueAlerts::default()
            }
        );
        state.record_alert_attempt(lag, now + 2);
        assert!(!state.due_alerts(now + 3).any());
        assert!(state.due_alerts(now + 60 * 60 * 1000 + 2).failure);
        assert!(state.due_alerts(now + 60 * 60 * 1000 + 2).lag);
        assert!(!state.due_alerts(now + 60 * 60 * 1000 + 2).legacy);
        assert!(state.due_alerts(now + 24 * 60 * 60 * 1000).legacy);
    }

    #[test]
    fn checkpoints_without_alert_categories_remain_readable() {
        let state = Checkpoint {
            last_error: Some("copy failed".into()),
            last_alert_ms: 123,
            ..Checkpoint::default()
        };
        let mut json = serde_json::to_value(state).unwrap();
        let object = json.as_object_mut().unwrap();
        for key in [
            "last_failure_alert_ms",
            "last_lag_alert_ms",
            "last_legacy_alert_ms",
        ] {
            object.remove(key);
        }
        let migrated: Checkpoint = serde_json::from_value(json).unwrap();
        assert_eq!(migrated.last_alert_ms, 123);
        assert_eq!(migrated.last_failure_alert_ms, 0);
        assert_eq!(migrated.last_lag_alert_ms, 0);
        assert_eq!(migrated.last_legacy_alert_ms, 0);
        assert!(migrated.due_alerts(124).failure);
    }

    #[test]
    fn oversized_empty_and_unknown_source_snapshots_are_rejected() {
        assert!(meta_with_csa_digest(&true, b"CSA").is_err());
        let mut oversized = snapshot("kifu", true);
        oversized.csa = vec![b'x'; MAX_CSA_BYTES + 1];
        assert!(oversized.validate().is_err());
        oversized.csa.clear();
        assert!(oversized.validate().is_err());
        let mut oversized = snapshot("kifu", true);
        oversized.meta_json = " ".repeat(MAX_META_BYTES + 1);
        assert!(oversized.validate().is_err());
        assert!(snapshot("unsupported", true).encode().is_err());
        assert!(Snapshot::decode("snapshots/v1/invalid", b"{truncated").is_err());
    }
}
