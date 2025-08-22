//! Configuration parameters for game phase detection

use crate::shogi::PieceType;

/// Minimum band width between thresholds to ensure stable classification
const MIN_BAND: f32 = 0.03;

/// Phase detection parameters
#[derive(Debug, Clone)]
pub struct PhaseParameters {
    /// Weight for material signal (0.0 - 1.0)
    pub w_material: f32,
    /// Weight for ply signal (0.0 - 1.0)
    /// Note: w_material + w_ply should equal 1.0
    pub w_ply: f32,

    /// Upper threshold (Middle→End boundary, 0.0 - 1.0)
    /// Higher values make EndGame classification less likely
    pub opening_threshold: f32,

    /// Lower threshold (Opening→Middle boundary, 0.0 - 1.0)
    /// Lower values make Opening classification less likely
    pub endgame_threshold: f32,

    /// Hysteresis for phase transitions to prevent oscillation
    pub hysteresis: f32,

    /// Ply at which opening typically ends
    pub ply_opening: u32,

    /// Ply at which endgame typically begins
    pub ply_endgame: u32,

    /// Piece value weights for material calculation
    pub phase_weights: PhaseWeights,
}

/// Weights for different piece types in phase calculation
#[derive(Debug, Clone)]
pub struct PhaseWeights {
    pub rook: u16,
    pub bishop: u16,
    pub gold: u16,
    pub silver: u16,
    pub knight: u16,
    pub lance: u16,
    // Pawn and King have implicit weight of 0
}

impl PhaseWeights {
    /// Get weight for a piece type
    pub fn get_weight(&self, piece_type: PieceType) -> u16 {
        match piece_type {
            PieceType::Rook => self.rook,
            PieceType::Bishop => self.bishop,
            PieceType::Gold => self.gold,
            PieceType::Silver => self.silver,
            PieceType::Knight => self.knight,
            PieceType::Lance => self.lance,
            PieceType::Pawn | PieceType::King => 0,
        }
    }

    /// Calculate initial total for normalization
    pub fn initial_total(&self) -> u16 {
        // Initial piece counts:
        // Rook: 2, Bishop: 2, Gold: 4, Silver: 4, Knight: 4, Lance: 4
        2 * self.rook
            + 2 * self.bishop
            + 4 * self.gold
            + 4 * self.silver
            + 4 * self.knight
            + 4 * self.lance
    }
}

/// Usage profile for phase detection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// For search engine (thread allocation)
    Search,
    /// For time management
    Time,
}

impl Default for PhaseWeights {
    fn default() -> Self {
        // Default weights matching controller.rs
        Self {
            rook: 4,
            bishop: 4,
            gold: 3,
            silver: 2,
            knight: 2,
            lance: 2,
        }
    }
}

impl PhaseParameters {
    /// Get parameters for a specific profile
    pub fn for_profile(profile: Profile) -> Self {
        match profile {
            Profile::Search => Self::search_profile(),
            Profile::Time => Self::time_profile(),
        }
    }

    /// Parameters optimized for search (material-heavy)
    fn search_profile() -> Self {
        let params = Self {
            w_material: 0.7,
            w_ply: 0.3,
            opening_threshold: 0.525, // Maps to ~(1-32/128)*0.7 = 0.525 in old system
            endgame_threshold: 0.176, // Maps to ~(1-96/128)*0.7 = 0.175 in old system, slightly adjusted
            hysteresis: 0.02,         // Smaller hysteresis
            ply_opening: 40,
            ply_endgame: 80,
            phase_weights: PhaseWeights::default(),
        };
        assert!(
            params.opening_threshold > params.endgame_threshold,
            "Opening threshold must be higher than endgame threshold"
        );
        assert!(
            params.opening_threshold - params.endgame_threshold >= MIN_BAND,
            "Band between thresholds must be at least {MIN_BAND}"
        );
        assert!(
            (params.w_material + params.w_ply - 1.0).abs() < 0.001,
            "Weights must sum to 1.0"
        );
        params
    }

    /// Parameters optimized for time management (ply-heavy)
    fn time_profile() -> Self {
        let params = Self {
            w_material: 0.3,
            w_ply: 0.7,
            opening_threshold: 0.50, // Upper threshold (Middle→End boundary)
            endgame_threshold: 0.20, // Lower threshold (Opening→Middle boundary)
            hysteresis: 0.05,
            ply_opening: 30,
            ply_endgame: 80,
            phase_weights: PhaseWeights::default(),
        };
        assert!(
            params.opening_threshold > params.endgame_threshold,
            "Opening threshold must be higher than endgame threshold"
        );
        assert!(
            params.opening_threshold - params.endgame_threshold >= MIN_BAND,
            "Band between thresholds must be at least {MIN_BAND}"
        );
        assert!(
            (params.w_material + params.w_ply - 1.0).abs() < 0.001,
            "Weights must sum to 1.0"
        );
        params
    }

    /// Create custom parameters
    pub fn custom(
        w_material: f32,
        w_ply: f32,
        opening_threshold: f32,
        endgame_threshold: f32,
    ) -> Self {
        assert!((w_material + w_ply - 1.0).abs() < 0.001, "Weights should sum to 1.0");
        assert!(
            opening_threshold > endgame_threshold,
            "Opening threshold should be higher than endgame"
        );
        assert!(
            opening_threshold - endgame_threshold >= MIN_BAND,
            "Band between thresholds must be at least {MIN_BAND}"
        );

        Self {
            w_material,
            w_ply,
            opening_threshold,
            endgame_threshold,
            hysteresis: 0.05,
            ply_opening: 40,
            ply_endgame: 80,
            phase_weights: PhaseWeights::default(),
        }
    }
}
