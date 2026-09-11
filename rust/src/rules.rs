//! Per-team-size game rules (`proxy_sv_teamSizeRules`).
//!
//! The proxy can override the server's `timelimit`, `fraglimit` and
//! `capturelimit` for the roster size currently fielded: an `N vs N` round uses
//! the rule defined for `N`. If no rule exists for `N`, the nearest smaller
//! defined size wins (5 vs 5 → 4 vs 4 → 3 vs 3 → …). When `N` is below every
//! defined size (e.g. 1 vs 1 with rules starting at 2), no rule applies and the
//! server's original limits are restored.
//!
//! The roster is reconciled on every frame, not only at the round-start
//! snapshot, so a game that becomes `2 vs 2` mid-match adopts the `2 vs 2`
//! rules and a `4 vs 4` adopts the `4 vs 4` ones. The limits are only re-written
//! when the *effective* rule changes, so a stable roster issues no trap calls.
//! This feature is independent of `proxy_sv_lockTeams`.
//!
//! `proxy_sv_teamSizeRules` syntax (rules separated by spaces/commas/semicolons,
//! fields by `:`):
//!
//! ```text
//! set proxy_sv_teamSizeRules "2:20:30:5,3:15:40:0"
//! ```
//!
//! i.e. `size:timelimit:fraglimit:capturelimit`. Any limit may be empty (or
//! `-`) to leave that cvar as-is, e.g. `4::40:` sets only `fraglimit`.

use core::ffi::CStr;
use std::ffi::CString;

use crate::sdk::{TEAM_BLUE, TEAM_RED};
use crate::state::{self, CVAR_TEAM_SIZE_RULES};
use crate::syscall;
use crate::teamlock;

/// One parsed `proxy_sv_teamSizeRules` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    /// Team size (`N` in `N vs N`).
    pub size: i32,
    pub time_limit: Option<i32>,
    pub frag_limit: Option<i32>,
    pub capture_limit: Option<i32>,
}

/// The server's `timelimit`/`fraglimit`/`capturelimit` captured before the
/// proxy first overrode them, so they can be restored once no rule applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseLimits {
    pub time_limit: i32,
    pub frag_limit: i32,
    pub capture_limit: i32,
}

/// What the per-frame reconciler should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing changed; no trap calls.
    None,
    /// Write the limits of this rule.
    Apply(Rule),
    /// Restore the captured server limits.
    Restore,
}

/// Parse the cvar value into rules. Malformed tokens/fields are ignored (a bad
/// field is treated as unset rather than discarding the whole rule).
pub fn parse(value: &str) -> Vec<Rule> {
    let mut rules = Vec::new();
    for token in value.split(|c: char| c == ',' || c == ';' || c.is_ascii_whitespace()) {
        if token.is_empty() {
            continue;
        }
        let parts: Vec<&str> = token.split(':').collect();
        let Some(size) = parts.first().and_then(|s| s.trim().parse::<i32>().ok()) else {
            continue;
        };
        if size <= 0 {
            continue;
        }
        rules.push(Rule {
            size,
            time_limit: parse_limit(parts.get(1).copied()),
            frag_limit: parse_limit(parts.get(2).copied()),
            capture_limit: parse_limit(parts.get(3).copied()),
        });
    }
    rules
}

/// One `:`-separated limit field: empty or `-` means "leave as-is".
fn parse_limit(field: Option<&str>) -> Option<i32> {
    match field.map(str::trim) {
        None | Some("") | Some("-") => None,
        Some(text) => text.parse::<i32>().ok(),
    }
}

/// The rule to apply for a round of `team_size` players per team: the defined
/// size nearest to (and not above) `team_size`. `None` when the roster is below
/// every defined size.
pub fn select(rules: &[Rule], team_size: i32) -> Option<Rule> {
    rules
        .iter()
        .filter(|rule| rule.size <= team_size)
        .max_by_key(|rule| rule.size)
        .copied()
}

/// Decide what to do with a `red` vs `blue` roster.
///
/// Only even, populated rosters (`red == blue >= 1`) are considered; uneven and
/// empty/FFA (`0 vs 0`) rosters keep the previous state. A change of the
/// effective rule applies it; an even roster below every defined rule restores
/// the captured server limits (`Restore`) once, if a rule had been applied.
pub fn decide(last: Option<Rule>, rules: &[Rule], red: i32, blue: i32) -> Action {
    if red < 1 || red != blue {
        return Action::None;
    }
    match select(rules, red) {
        Some(rule) if last != Some(rule) => Action::Apply(rule),
        Some(_) => Action::None,
        None if last.is_some() => Action::Restore,
        None => Action::None,
    }
}

/// Reconcile the per-team-size rules with the roster of the current frame.
///
/// Called every `GAME_RUN_FRAME` (independently of the team lock): computes the
/// team counts, resolves the effective rule, and applies the rule, restores the
/// server limits, or does nothing when neither changed.
pub fn on_run_frame() {
    let (red, blue) = (
        teamlock::team_count(TEAM_RED),
        teamlock::team_count(TEAM_BLUE),
    );
    let value = cvar_value();
    let rules = parse(&value);
    let last = state::with_state(|s| s.last_team_rule);
    match decide(last, &rules, red, blue) {
        Action::None => {}
        Action::Apply(rule) => {
            capture_base_limits();
            eprintln!(
                "----- proxy-rs: rules apply {red} vs {blue} size={} time={:?} frag={:?} capture={:?}",
                rule.size, rule.time_limit, rule.frag_limit, rule.capture_limit
            );
            set_limit(c"timelimit", rule.time_limit);
            set_limit(c"fraglimit", rule.frag_limit);
            set_limit(c"capturelimit", rule.capture_limit);
            state::with_state(|s| s.last_team_rule = Some(rule));
        }
        Action::Restore => {
            let base = restore_base_limits();
            eprintln!("----- proxy-rs: rules restore {red} vs {blue} -> {base:?}");
            state::with_state(|s| s.last_team_rule = None);
        }
    }
}

/// Capture the server's current limits once, before the proxy first overrides
/// them.
fn capture_base_limits() {
    if state::with_state(|s| s.base_limits.is_some()) {
        return;
    }
    // SAFETY: engine syscall registered (GAME_INIT ran before any frame).
    let base = unsafe {
        BaseLimits {
            time_limit: syscall::cvar_variable_integer_value(c"timelimit"),
            frag_limit: syscall::cvar_variable_integer_value(c"fraglimit"),
            capture_limit: syscall::cvar_variable_integer_value(c"capturelimit"),
        }
    };
    state::with_state(|s| s.base_limits = Some(base));
}

/// Write the captured server limits back (no-op when nothing was captured).
fn restore_base_limits() -> Option<BaseLimits> {
    let base = state::with_state(|s| s.base_limits)?;
    set_limit(c"timelimit", Some(base.time_limit));
    set_limit(c"fraglimit", Some(base.frag_limit));
    set_limit(c"capturelimit", Some(base.capture_limit));
    Some(base)
}

/// Restore the captured limits on module shutdown (map change / `map_restart`)
/// so a rule value does not leak into the next map (game cvars persist across
/// maps). No-op when no rule is currently applied.
pub fn restore_on_shutdown() {
    if state::with_state(|s| s.last_team_rule.is_none()) {
        return;
    }
    restore_base_limits();
    state::with_state(|s| {
        s.last_team_rule = None;
        s.base_limits = None;
    });
}

/// The `proxy_sv_teamSizeRules` value read out of the cvar mirror.
fn cvar_value() -> String {
    state::with_state(|s| {
        let raw = &s.cvars[CVAR_TEAM_SIZE_RULES].string;
        let bytes: Vec<u8> = raw
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// `trap_Cvar_Set(name, value)` for a defined limit only.
fn set_limit(name: &CStr, value: Option<i32>) {
    let Some(value) = value else {
        return;
    };
    let Ok(text) = CString::new(value.to_string()) else {
        return;
    };
    // SAFETY: engine syscall registered; name is a static literal and text is
    // NUL-terminated.
    unsafe { syscall::cvar_set(name, &text) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes_and_limits() {
        let rules = parse("2:20:30:5,3:15:40:0");
        assert_eq!(
            rules,
            vec![
                Rule {
                    size: 2,
                    time_limit: Some(20),
                    frag_limit: Some(30),
                    capture_limit: Some(5),
                },
                Rule {
                    size: 3,
                    time_limit: Some(15),
                    frag_limit: Some(40),
                    capture_limit: Some(0),
                },
            ]
        );
    }

    #[test]
    fn empty_and_dash_fields_are_unset() {
        assert_eq!(
            parse("4::40:"),
            vec![Rule {
                size: 4,
                time_limit: None,
                frag_limit: Some(40),
                capture_limit: None,
            }]
        );
        assert_eq!(
            parse("4:-:40:-"),
            vec![Rule {
                size: 4,
                time_limit: None,
                frag_limit: Some(40),
                capture_limit: None,
            }]
        );
        // Trailing fields may be omitted entirely.
        assert_eq!(
            parse("2:20"),
            vec![Rule {
                size: 2,
                time_limit: Some(20),
                frag_limit: None,
                capture_limit: None,
            }]
        );
    }

    #[test]
    fn whitespace_separates_rules() {
        assert_eq!(parse("2:20:30:5 3:15:40:0").len(), 2);
        assert_eq!(parse("2:20:30:5;3:15:40:0").len(), 2);
    }

    #[test]
    fn malformed_tokens_are_ignored() {
        assert!(parse("").is_empty());
        assert!(parse("abc").is_empty());
        assert!(parse("0:1:2:3").is_empty());
        assert!(parse("-2:1:2:3").is_empty());
        assert!(parse("2").len() == 1);
        // Bad field becomes unset, rule kept.
        assert_eq!(
            parse("2:x:30"),
            vec![Rule {
                size: 2,
                time_limit: None,
                frag_limit: Some(30),
                capture_limit: None,
            }]
        );
    }

    #[test]
    fn select_takes_nearest_lower_size() {
        let rules = parse("2:20:30:5,4:15:40:0");
        // Exact match.
        assert_eq!(select(&rules, 4).unwrap().time_limit, Some(15));
        // 5 vs 5 falls back to the 4 vs 4 rule.
        assert_eq!(select(&rules, 5).unwrap().size, 4);
        // 3 vs 3 falls back to the 2 vs 2 rule.
        assert_eq!(select(&rules, 3).unwrap().size, 2);
        // Below every defined size: no rule.
        assert_eq!(select(&rules, 1), None);
        // Rules above the current size never apply.
        let big = parse("5:10:10:10");
        assert_eq!(select(&big, 4), None);
        assert_eq!(select(&big, 5).unwrap().size, 5);
        assert_eq!(select(&big, 6).unwrap().size, 5);
    }

    #[test]
    fn select_prefers_downward_between_sizes() {
        let rules = parse("2:20:30:5,6:15:40:0");
        // 5 vs 5 is closer to 6, but the fallback is strictly downward.
        assert_eq!(select(&rules, 5).unwrap().size, 2);
        assert_eq!(select(&rules, 6).unwrap().size, 6);
    }

    #[test]
    fn empty_rules_select_nothing() {
        assert_eq!(select(&parse(""), 4), None);
    }

    #[test]
    fn decide_applies_changes_and_restores_below_the_smallest_rule() {
        let rules = parse("2:1:0:0,3:0:1:0");
        let rule2 = select(&rules, 2);
        let rule3 = select(&rules, 3);
        // First sighting of 2 vs 2 applies its rule.
        assert_eq!(decide(None, &rules, 2, 2), Action::Apply(rule2.unwrap()));
        // A stable roster does nothing.
        assert_eq!(decide(rule2, &rules, 2, 2), Action::None);
        // 3 vs 3 switches to the 3 vs 3 rule.
        assert_eq!(decide(rule2, &rules, 3, 3), Action::Apply(rule3.unwrap()));
        // 4 vs 4 falls back to the 3 vs 3 rule (unchanged).
        assert_eq!(decide(rule3, &rules, 4, 4), Action::None);
        // 1 vs 1 has no rule: restore the server limits.
        assert_eq!(decide(rule3, &rules, 1, 1), Action::Restore);
        // With nothing applied there is nothing to restore.
        assert_eq!(decide(None, &rules, 1, 1), Action::None);
        // Uneven and empty/FFA rosters keep the previous state.
        assert_eq!(decide(rule2, &rules, 2, 3), Action::None);
        assert_eq!(decide(None, &rules, 0, 0), Action::None);
    }

    #[test]
    fn decide_honours_an_explicit_one_vs_one_rule() {
        let rules = parse("1:5:5:5,2:1:0:0");
        let rule1 = select(&rules, 1);
        assert_eq!(
            decide(None, &rules, 1, 1),
            Action::Apply(rule1.unwrap()),
            "an explicit 1 vs 1 rule must apply"
        );
    }
}
