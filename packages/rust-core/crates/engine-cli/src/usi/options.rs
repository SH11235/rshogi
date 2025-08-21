//! USI engine options

use std::fmt;

/// Engine option types
#[derive(Debug, Clone)]
pub enum EngineOption {
    /// Checkbox option
    Check { name: String, default: bool },

    /// Spin (numeric) option
    Spin {
        name: String,
        default: i64,
        min: i64,
        max: i64,
    },

    /// Combo (dropdown) option
    Combo {
        name: String,
        default: String,
        options: Vec<String>,
    },

    /// Filename option
    Filename { name: String, default: String },

    /// Button option (action trigger)
    Button { name: String },
}

impl EngineOption {
    /// Create a check option
    pub fn check(name: impl Into<String>, default: bool) -> Self {
        EngineOption::Check {
            name: name.into(),
            default,
        }
    }

    /// Create a spin option
    pub fn spin(name: impl Into<String>, default: i64, min: i64, max: i64) -> Self {
        EngineOption::Spin {
            name: name.into(),
            default,
            min,
            max,
        }
    }

    /// Create a combo option
    pub fn combo(name: impl Into<String>, default: String, options: Vec<String>) -> Self {
        EngineOption::Combo {
            name: name.into(),
            default,
            options,
        }
    }

    /// Create a filename option
    pub fn filename(name: impl Into<String>, default: String) -> Self {
        EngineOption::Filename {
            name: name.into(),
            default,
        }
    }

    /// Create a button option
    pub fn button(name: impl Into<String>) -> Self {
        EngineOption::Button { name: name.into() }
    }
}

impl fmt::Display for EngineOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineOption::Check { name, default } => {
                write!(f, "option name {name} type check default {default}")
            }
            EngineOption::Spin {
                name,
                default,
                min,
                max,
            } => {
                write!(f, "option name {name} type spin default {default} min {min} max {max}")
            }
            EngineOption::Combo {
                name,
                default,
                options,
            } => {
                write!(f, "option name {name} type combo default {default}")?;
                for opt in options {
                    write!(f, " var {opt}")?;
                }
                Ok(())
            }
            EngineOption::Filename { name, default } => {
                write!(f, "option name {name} type filename default {default}")
            }
            EngineOption::Button { name } => {
                write!(f, "option name {name} type button")
            }
        }
    }
}
