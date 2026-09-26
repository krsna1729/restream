//! glibc malloc arena policy, applied once at startup before any thread
//! exists, recorded, and reported (startup log and host-settings telemetry).
//!
//! glibc grows up to eight arenas per core and keeps each arena's freed
//! memory resident. On the 6-CPU capacity host (Restream on 3 pinned CPUs),
//! capping at 2 arenas lowered steady RSS by 34 MB at 50 SRT outputs and
//! 47 MB at 500 RTMP outputs, halved the frozen-SRT-destination surge (66–72
//! → 30–47 MB), with no measurable CPU change. Arenas exist to reduce
//! allocator lock contention, so the right value can differ on hosts with
//! many more cores and threads: **2 is a provisional default**, qualified per
//! host with `RESTREAM_MALLOC_ARENA_MAX` (see `docs/capacity-ramp.md`).
//!
//! Precedence: an operator's `MALLOC_ARENA_MAX` (read by glibc itself) wins;
//! then `RESTREAM_MALLOC_ARENA_MAX` (`default` leaves glibc's policy, a
//! positive integer sets it); otherwise the provisional default applies.

use std::sync::OnceLock;

/// The provisional default arena cap (see the module docs).
pub const PROVISIONAL_ARENA_MAX: i32 = 2;

/// What startup decided to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArenaDecision {
    /// `MALLOC_ARENA_MAX` is set; glibc applies it itself.
    OperatorGlibcEnv(String),
    /// `RESTREAM_MALLOC_ARENA_MAX=default`: leave glibc's own policy.
    GlibcDefault,
    /// Cap arenas at this count.
    Set { arenas: i32, provisional: bool },
}

/// What actually took effect, recorded for logging and telemetry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArenaSetting {
    OperatorGlibcEnv(String),
    GlibcDefault,
    Set {
        arenas: i32,
        provisional: bool,
    },
    /// `mallopt` refused the value; glibc's default policy stays in force.
    Failed {
        arenas: i32,
    },
    /// Not a glibc target: no arena policy to set.
    Unsupported,
    /// `RESTREAM_MALLOC_ARENA_MAX` was not `default` or a positive integer;
    /// the provisional default was applied instead.
    InvalidOverride {
        value: String,
        applied: Box<ArenaSetting>,
    },
}

static APPLIED: OnceLock<ArenaSetting> = OnceLock::new();

/// Pure precedence rule, separated for tests.
pub fn decide(
    malloc_arena_max: Option<&str>,
    restream_override: Option<&str>,
) -> Result<ArenaDecision, String> {
    if let Some(value) = malloc_arena_max {
        return Ok(ArenaDecision::OperatorGlibcEnv(value.to_string()));
    }
    match restream_override.map(str::trim) {
        None | Some("") => Ok(ArenaDecision::Set {
            arenas: PROVISIONAL_ARENA_MAX,
            provisional: true,
        }),
        Some(value) if value.eq_ignore_ascii_case("default") => Ok(ArenaDecision::GlibcDefault),
        Some(value) => match value.parse::<i32>() {
            Ok(arenas) if arenas > 0 => Ok(ArenaDecision::Set {
                arenas,
                provisional: false,
            }),
            _ => Err(value.to_string()),
        },
    }
}

/// Apply the arena policy from the environment. Call once from `main` before
/// any thread starts; later calls return the first result.
pub fn apply_from_env() -> &'static ArenaSetting {
    APPLIED.get_or_init(|| {
        let (glibc, override_value) = crate::config::malloc_arena_env();
        match decide(glibc.as_deref(), override_value.as_deref()) {
            Ok(decision) => apply(decision),
            Err(value) => ArenaSetting::InvalidOverride {
                value,
                applied: Box::new(apply(ArenaDecision::Set {
                    arenas: PROVISIONAL_ARENA_MAX,
                    provisional: true,
                })),
            },
        }
    })
}

/// The recorded setting, once `apply_from_env` has run.
pub fn applied() -> Option<&'static ArenaSetting> {
    APPLIED.get()
}

fn apply(decision: ArenaDecision) -> ArenaSetting {
    match decision {
        ArenaDecision::OperatorGlibcEnv(value) => ArenaSetting::OperatorGlibcEnv(value),
        ArenaDecision::GlibcDefault => ArenaSetting::GlibcDefault,
        ArenaDecision::Set {
            arenas,
            provisional,
        } => set_arena_max(arenas, provisional),
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn set_arena_max(arenas: i32, provisional: bool) -> ArenaSetting {
    // SAFETY: mallopt only adjusts allocator tuning; `apply_from_env` runs on
    // the main thread before any other thread exists. It returns 1 on success.
    if unsafe { libc::mallopt(libc::M_ARENA_MAX, arenas) } == 1 {
        ArenaSetting::Set {
            arenas,
            provisional,
        }
    } else {
        ArenaSetting::Failed { arenas }
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn set_arena_max(_arenas: i32, _provisional: bool) -> ArenaSetting {
    ArenaSetting::Unsupported
}

impl std::fmt::Display for ArenaSetting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OperatorGlibcEnv(value) => {
                write!(f, "{value} (MALLOC_ARENA_MAX, applied by glibc)")
            }
            Self::GlibcDefault => f.write_str("glibc default (RESTREAM_MALLOC_ARENA_MAX=default)"),
            Self::Set {
                arenas,
                provisional: true,
            } => write!(
                f,
                "{arenas} (provisional default; cross-host qualification pending)"
            ),
            Self::Set { arenas, .. } => write!(f, "{arenas} (RESTREAM_MALLOC_ARENA_MAX)"),
            Self::Failed { arenas } => {
                write!(f, "glibc default (mallopt(M_ARENA_MAX, {arenas}) failed)")
            }
            Self::Unsupported => f.write_str("not applicable (non-glibc target)"),
            Self::InvalidOverride { value, applied } => {
                write!(
                    f,
                    "{applied} (ignored invalid RESTREAM_MALLOC_ARENA_MAX={value:?})"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operator_glibc_env_wins() {
        assert_eq!(
            decide(Some("8"), Some("2")),
            Ok(ArenaDecision::OperatorGlibcEnv("8".into()))
        );
    }

    #[test]
    fn restream_override_selects_default_or_a_count() {
        assert_eq!(
            decide(None, Some("default")),
            Ok(ArenaDecision::GlibcDefault)
        );
        assert_eq!(
            decide(None, Some("4")),
            Ok(ArenaDecision::Set {
                arenas: 4,
                provisional: false
            })
        );
        assert_eq!(decide(None, Some("0")), Err("0".into()));
        assert_eq!(decide(None, Some("lots")), Err("lots".into()));
    }

    #[test]
    fn unset_uses_the_provisional_default() {
        assert_eq!(
            decide(None, None),
            Ok(ArenaDecision::Set {
                arenas: PROVISIONAL_ARENA_MAX,
                provisional: true
            })
        );
    }

    #[test]
    fn settings_describe_themselves_for_operators() {
        let failed = ArenaSetting::Failed { arenas: 2 }.to_string();
        assert!(failed.contains("failed"), "{failed}");
        let provisional = ArenaSetting::Set {
            arenas: 2,
            provisional: true,
        }
        .to_string();
        assert!(provisional.contains("provisional"), "{provisional}");
    }
}
