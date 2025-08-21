//! Option management functionality for the engine adapter.
//!
//! This module handles USI option initialization and setting,
//! including engine configuration, time management parameters,
//! and various tuning options.

use anyhow::{anyhow, Context, Result};
use engine_core::engine::controller::EngineType;
use log::{debug, info, warn};

use crate::engine_adapter::EngineAdapter;
use crate::usi::{
    clamp_periods, EngineOption, MAX_BYOYOMI_PERIODS, MIN_BYOYOMI_PERIODS, OPT_BYOYOMI_OVERHEAD_MS,
    OPT_BYOYOMI_PERIODS, OPT_BYOYOMI_SAFETY_MS, OPT_OVERHEAD_MS, OPT_USI_BYOYOMI_PERIODS,
};
use engine_core::time_management::constants::{
    DEFAULT_BYOYOMI_OVERHEAD_MS, DEFAULT_BYOYOMI_SAFETY_MS, DEFAULT_OVERHEAD_MS,
    MAX_BYOYOMI_SAFETY_MS, MAX_OVERHEAD_MS, MIN_BYOYOMI_SAFETY_MS, MIN_OVERHEAD_MS,
};

impl EngineAdapter {
    /// Initialize engine options
    pub(super) fn init_options(&mut self) {
        self.options = vec![
            EngineOption::spin("USI_Hash", 16, 1, 1024),
            EngineOption::spin("Threads", 1, 1, 256),
            EngineOption::check("USI_Ponder", true),
            EngineOption::combo(
                "EngineType",
                "Material".to_string(), // Default to Material for stability
                vec![
                    "Material".to_string(),
                    "Enhanced".to_string(), // Put Enhanced before NNUE types
                    "Nnue".to_string(),
                    "EnhancedNnue".to_string(),
                ],
            ),
            EngineOption::filename("EvalFile", "".to_string()), // Add EvalFile option
            EngineOption::spin(
                OPT_BYOYOMI_PERIODS,
                1,
                MIN_BYOYOMI_PERIODS as i64,
                MAX_BYOYOMI_PERIODS as i64,
            ),
            EngineOption::spin("ByoyomiEarlyFinishRatio", 80, 50, 95),
            EngineOption::spin("PVStabilityBase", 80, 10, 200),
            EngineOption::spin("PVStabilitySlope", 5, 0, 20),
            EngineOption::spin(
                OPT_OVERHEAD_MS,
                DEFAULT_OVERHEAD_MS as i64,
                MIN_OVERHEAD_MS as i64,
                MAX_OVERHEAD_MS as i64,
            ),
            EngineOption::spin(
                OPT_BYOYOMI_OVERHEAD_MS,
                DEFAULT_BYOYOMI_OVERHEAD_MS as i64,
                MIN_OVERHEAD_MS as i64,
                MAX_OVERHEAD_MS as i64,
            ),
            EngineOption::spin(
                OPT_BYOYOMI_SAFETY_MS,
                DEFAULT_BYOYOMI_SAFETY_MS as i64,
                MIN_BYOYOMI_SAFETY_MS as i64,
                MAX_BYOYOMI_SAFETY_MS as i64,
            ),
        ];
    }

    /// Get available options
    pub fn get_options(&self) -> &[EngineOption] {
        &self.options
    }

    /// Initialize the engine
    pub fn initialize(&mut self) -> Result<()> {
        // Apply thread count and hash size to engine
        if let Some(ref mut engine) = self.engine {
            engine.set_threads(self.threads);
            engine.set_hash_size(self.hash_size);
        }
        Ok(())
    }

    /// Helper function to parse u64 with range check
    fn parse_u64_in_range(name: &str, val: &str, min: u64, max: u64) -> Result<u64> {
        let v = val.parse::<u64>().with_context(|| format!("Invalid {name}: '{val}'"))?;
        if !(min..=max).contains(&v) {
            anyhow::bail!("{name} must be between {min} and {max}, got {v}");
        }
        Ok(v)
    }

    /// Set engine option
    pub fn set_option(&mut self, name: &str, value: Option<&str>) -> Result<()> {
        match name {
            "USI_Hash" => {
                if let Some(val) = value {
                    let hash_size = val.parse::<usize>().map_err(|_| {
                        anyhow!("Invalid hash size: '{}'. Must be a number between 1 and 1024", val)
                    })?;
                    self.hash_size = hash_size;

                    // Apply to engine if it exists
                    if let Some(ref mut engine) = self.engine {
                        engine.set_hash_size(hash_size);
                    } else {
                        info!("Hash size option queued: {hash_size}MB");
                    }
                }
            }
            "Threads" => {
                if let Some(val) = value {
                    let threads = val.parse::<usize>().map_err(|_| {
                        anyhow!(
                            "Invalid thread count: '{}'. Must be a number between 1 and 256",
                            val
                        )
                    })?;
                    self.threads = threads;

                    // Apply to engine if it exists
                    if let Some(ref mut engine) = self.engine {
                        engine.set_threads(threads);
                    } else {
                        info!("Threads option queued: {threads}");
                    }
                }
            }
            "USI_Ponder" => {
                if let Some(val) = value {
                    self.ponder = val.to_lowercase() == "true";
                }
            }
            "EngineType" => {
                if let Some(val) = value {
                    let engine_type = match val {
                        "Material" => EngineType::Material,
                        "Nnue" => EngineType::Nnue,
                        "Enhanced" => EngineType::Enhanced,
                        "EnhancedNnue" => EngineType::EnhancedNnue,
                        _ => return Err(anyhow!("Invalid engine type: '{}'. Valid values are: Material, Nnue, Enhanced, EnhancedNnue", val)),
                    };
                    if let Some(ref mut engine) = self.engine {
                        engine.set_engine_type(engine_type);
                        // Re-apply thread count after engine type change
                        engine.set_threads(self.threads);
                    } else {
                        self.pending_engine_type = Some(engine_type);
                        info!("EngineType option queued: {engine_type:?}");
                    }
                }
            }
            OPT_BYOYOMI_PERIODS | OPT_USI_BYOYOMI_PERIODS => {
                if let Some(val) = value {
                    if val == "default" {
                        self.byoyomi_periods = None;
                    } else {
                        let periods = val.parse::<u32>().map_err(|_| {
                            anyhow!(
                                "Invalid {}: '{}'. Must be a number between {} and {} or 'default'",
                                OPT_BYOYOMI_PERIODS,
                                val,
                                MIN_BYOYOMI_PERIODS,
                                MAX_BYOYOMI_PERIODS
                            )
                        })?;
                        self.byoyomi_periods = Some(clamp_periods(periods, false));
                    }
                } else {
                    self.byoyomi_periods = None;
                }
            }
            "ByoyomiEarlyFinishRatio" => {
                if let Some(val_str) = value {
                    let ratio = val_str.parse::<u8>().with_context(|| {
                        format!("Invalid value for ByoyomiEarlyFinishRatio: '{val_str}'. Expected integer 50-95")
                    })?;
                    if !(50..=95).contains(&ratio) {
                        return Err(anyhow!("ByoyomiEarlyFinishRatio must be between 50 and 95"));
                    }
                    self.byoyomi_early_finish_ratio = ratio;
                }
            }
            "PVStabilityBase" => {
                if let Some(val_str) = value {
                    let base = val_str.parse::<u64>().with_context(|| {
                        format!(
                            "Invalid value for PVStabilityBase: '{val_str}'. Expected integer 10-200"
                        )
                    })?;
                    if !(10..=200).contains(&base) {
                        return Err(anyhow!("PVStabilityBase must be between 10 and 200"));
                    }
                    self.pv_stability_base = base;
                }
            }
            "PVStabilitySlope" => {
                if let Some(val_str) = value {
                    let slope = val_str.parse::<u64>().with_context(|| {
                        format!(
                            "Invalid value for PVStabilitySlope: '{val_str}'. Expected integer 0-20"
                        )
                    })?;
                    if slope > 20 {
                        return Err(anyhow!("PVStabilitySlope must be between 0 and 20"));
                    }
                    self.pv_stability_slope = slope;
                }
            }
            "EvalFile" => {
                if let Some(path) = value {
                    if !path.is_empty() {
                        // Only load NNUE weights if using NNUE engine type
                        if let Some(ref mut engine) = self.engine {
                            let engine_type = engine.get_engine_type();
                            if matches!(engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
                                info!("Loading NNUE weights from: {path}");
                                match engine.load_nnue_weights(path) {
                                    Ok(()) => {
                                        info!("NNUE weights loaded successfully");
                                    }
                                    Err(e) => {
                                        log::error!("Failed to load NNUE weights: {e}");
                                        return Err(anyhow!(
                                            "Failed to load NNUE weights from '{}': {}",
                                            path,
                                            e
                                        ));
                                    }
                                }
                            } else {
                                debug!(
                                    "EvalFile option ignored for non-NNUE engine type: {engine_type:?}"
                                );
                            }
                        } else {
                            self.pending_eval_file = Some(path.to_string());
                            info!("EvalFile option queued: {path}");
                        }
                    }
                }
            }
            OPT_OVERHEAD_MS => {
                if let Some(val) = value {
                    self.overhead_ms = Self::parse_u64_in_range(
                        OPT_OVERHEAD_MS,
                        val,
                        MIN_OVERHEAD_MS,
                        MAX_OVERHEAD_MS,
                    )?;
                }
            }
            OPT_BYOYOMI_OVERHEAD_MS => {
                if let Some(val) = value {
                    self.byoyomi_overhead_ms = Self::parse_u64_in_range(
                        OPT_BYOYOMI_OVERHEAD_MS,
                        val,
                        MIN_OVERHEAD_MS,
                        MAX_OVERHEAD_MS,
                    )?;
                }
            }
            OPT_BYOYOMI_SAFETY_MS => {
                if let Some(val) = value {
                    self.byoyomi_safety_ms = Self::parse_u64_in_range(
                        OPT_BYOYOMI_SAFETY_MS,
                        val,
                        MIN_BYOYOMI_SAFETY_MS,
                        MAX_BYOYOMI_SAFETY_MS,
                    )?;
                }
            }
            _ => {
                warn!("Unknown option '{name}' ignored for compatibility");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usi_hash_option() {
        let mut adapter = EngineAdapter::new();

        // Initialize with Material engine
        adapter.set_option("EngineType", Some("Material")).unwrap();
        adapter.initialize().unwrap();

        // Check initial hash size
        assert_eq!(adapter.hash_size, 16); // default

        // Set new hash size via USI option
        adapter.set_option("USI_Hash", Some("64")).unwrap();

        // Hash size should be updated in adapter
        assert_eq!(adapter.hash_size, 64);

        // Take and return engine to verify it applies
        if let Ok(engine) = adapter.take_engine() {
            // Engine should have pending hash size
            assert_eq!(engine.get_hash_size(), 16); // Still old size until applied
            adapter.return_engine(engine);
        }

        // After return, engine should have new hash size set as pending
        if let Some(ref engine) = adapter.engine {
            // Next search will apply the pending size
            assert_eq!(engine.get_hash_size(), 16); // Still 16 until next search
        }
    }

    #[test]
    fn test_usi_hash_invalid_values() {
        let mut adapter = EngineAdapter::new();

        // Test invalid number
        let result = adapter.set_option("USI_Hash", Some("not_a_number"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid hash size"));

        // Test empty value
        let result = adapter.set_option("USI_Hash", None);
        assert!(result.is_ok()); // None is OK, just doesn't change anything
    }

    #[test]
    fn test_hash_size_queued_before_engine_init() {
        let mut adapter = EngineAdapter::new();

        // Set hash size before engine exists
        adapter.set_option("USI_Hash", Some("128")).unwrap();
        assert_eq!(adapter.hash_size, 128);

        // Initialize engine
        adapter.set_option("EngineType", Some("Enhanced")).unwrap();
        adapter.initialize().unwrap();

        // Engine should be created with the queued hash size
        if let Some(ref engine) = adapter.engine {
            // Hash size should be set as pending in the engine
            assert_eq!(engine.get_hash_size(), 16); // Default until applied
        }
    }
}
