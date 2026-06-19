//! EngineKind enum + EngineCaps — platform token and availability descriptor.

use lightr_core::LightrError;

// ── EngineKind ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Native,
    Ns,
    Vz,
    /// Windows isolation engine: runs the `ns` model inside the default WSL2
    /// distro's utility VM. Analog of `Vz` (macOS) / `Ns` (Linux). The WSL2 VM
    /// is the OS's, not ours — so "no daemon" still holds.
    Wsl,
}

impl std::str::FromStr for EngineKind {
    type Err = LightrError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "native" => Ok(EngineKind::Native),
            "ns" => Ok(EngineKind::Ns),
            "vz" => Ok(EngineKind::Vz),
            "wsl" => Ok(EngineKind::Wsl),
            _ => Err(LightrError::InvalidRef(format!("unknown engine: {s}"))),
        }
    }
}

impl EngineKind {
    /// Stable lowercase token (inverse of `FromStr`). Stable across cfg so
    /// `engine ls` can render every kind on every platform.
    pub fn as_str(self) -> &'static str {
        match self {
            EngineKind::Native => "native",
            EngineKind::Ns => "ns",
            EngineKind::Vz => "vz",
            EngineKind::Wsl => "wsl",
        }
    }

    /// Every engine kind, in display order. Provided so callers (e.g. the CLI's
    /// `engine ls`) can iterate without re-hardcoding the variant set and
    /// without an exhaustive `match` that breaks when a kind is added.
    pub fn all() -> &'static [EngineKind] {
        &[
            EngineKind::Native,
            EngineKind::Ns,
            EngineKind::Vz,
            EngineKind::Wsl,
        ]
    }

    /// The isolation engine this platform selects by default:
    /// macOS → `Vz`, Linux → `Ns`, Windows → `Wsl`, else `Native`.
    /// `Native` always works everywhere; the platform isolation engine reports
    /// its own honest availability via [`probe`].
    pub fn platform_default() -> EngineKind {
        #[cfg(target_os = "macos")]
        {
            EngineKind::Vz
        }
        #[cfg(target_os = "linux")]
        {
            EngineKind::Ns
        }
        #[cfg(target_os = "windows")]
        {
            EngineKind::Wsl
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            EngineKind::Native
        }
    }
}

// ── EngineCaps ────────────────────────────────────────────────────────────────

pub struct EngineCaps {
    pub available: bool,
    pub detail: String,
}
