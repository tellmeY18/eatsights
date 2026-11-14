//! Trust policy (ADR-0006, ADR-0007): who may grant what, and a deterministic
//! recompute of every key's level and depth from the private attestation graph.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};

/// Trust levels, ordered from lowest to highest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Unverified,
    Member,
    Trusted,
    Verifier,
    Root,
}

impl Level {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "unverified" => Self::Unverified,
            "member" => Self::Member,
            "trusted" => Self::Trusted,
            "verifier" => Self::Verifier,
            "root" => Self::Root,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Member => "member",
            Self::Trusted => "trusted",
            Self::Verifier => "verifier",
            Self::Root => "root",
        }
    }

    /// One level lower, never below `member` (strike demotion). Root is exempt.
    pub fn demoted(self) -> Self {
        match self {
            Self::Root => Self::Root,
            Self::Verifier => Self::Trusted,
            _ => Self::Member,
        }
    }

    /// Published keys: root and verifier.
    pub fn is_published(self) -> bool {
        self >= Self::Verifier
    }
}

/// `issuer` may grant `granted` by vouch: the issuer must be strictly above, and
/// only `member`, `trusted` and `verifier` can be granted. This gives
/// member <- trusted/verifier/root, trusted <- verifier/root, verifier <- root.
pub fn can_grant(issuer: Level, granted: Level) -> bool {
    matches!(granted, Level::Member | Level::Trusted | Level::Verifier) && issuer > granted
}

/// Numbers from ADR-0006. All configurable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustConfig {
    pub max_depth: i32,
    pub vendor_of_max_days: i64,
    pub strikes: i64,
    pub strike_window_days: i64,
    pub vouch_cooldown_days: i64,
}

impl Default for TrustConfig {
    fn default() -> Self {
        Self {
            max_depth: 4,
            vendor_of_max_days: 365,
            strikes: 2,
            strike_window_days: 180,
            vouch_cooldown_days: 30,
        }
    }
}

/// One key as the recompute sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyNode {
    pub key_id: String,
    /// `active`, `revoked` or `rotated`.
    pub status: String,
    /// Fixed root (configured, not derived from attestations).
    pub is_root: bool,
    /// Strike cap: vouches received at or before `demoted_at` grant at most this.
    pub level_cap: Option<Level>,
    pub demoted_at: Option<DateTime<Utc>>,
}

/// An unrevoked, unexpired `vouch` or `rotate` edge.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub issuer: String,
    pub subject: String,
    /// `Some(level)` for a vouch; `None` for a rotate (subject inherits issuer).
    pub granted: Option<Level>,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Standing {
    pub level: Level,
    /// `None` for unverified keys.
    pub depth: Option<i32>,
}

/// Recompute every key's level and depth. Pure and deterministic: the result does
/// not depend on the order of `keys` or `edges`.
///
/// - Root keys are `root`, depth 0.
/// - A vouch counts while its issuer is not `revoked` (a rotated issuer keeps its
///   standing for vouches it issued before rotating), the issuer's computed level
///   may grant the level (`can_grant`) and issuer depth + 1 <= `max_depth`.
/// - A rotate edge passes the old key's level and depth to the new key unchanged.
/// - A key takes the highest level offered (then the smallest depth), capped by its
///   strike cap for vouches received up to its demotion.
pub fn recompute(keys: &[KeyNode], edges: &[Edge], max_depth: i32) -> BTreeMap<String, Standing> {
    let nodes: HashMap<&str, &KeyNode> = keys.iter().map(|k| (k.key_id.as_str(), k)).collect();
    let mut st: BTreeMap<String, Standing> = keys
        .iter()
        .map(|k| {
            let s = if k.is_root && k.status == "active" {
                Standing {
                    level: Level::Root,
                    depth: Some(0),
                }
            } else {
                Standing {
                    level: Level::Unverified,
                    depth: None,
                }
            };
            (k.key_id.clone(), s)
        })
        .collect();
    // Monotone fixed point: standings only rise, bounded by levels x depths.
    let rank = |s: &Standing| (s.level, std::cmp::Reverse(s.depth.unwrap_or(i32::MAX)));
    for _ in 0..=(keys.len() * 5 + 5) {
        let mut changed = false;
        for e in edges {
            let (Some(issuer), Some(subject)) =
                (nodes.get(e.issuer.as_str()), nodes.get(e.subject.as_str()))
            else {
                continue;
            };
            if issuer.status == "revoked" || subject.is_root {
                continue;
            }
            let Some(is) = st.get(&issuer.key_id).copied() else {
                continue;
            };
            let Some(idepth) = is.depth else { continue };
            let offer = match e.granted {
                Some(g) => {
                    if !can_grant(is.level, g) || idepth + 1 > max_depth {
                        continue;
                    }
                    let capped = match (subject.level_cap, subject.demoted_at) {
                        (Some(cap), Some(at)) if e.received_at <= at => g.min(cap),
                        _ => g,
                    };
                    Standing {
                        level: capped,
                        depth: Some(idepth + 1),
                    }
                }
                None => {
                    if is.level == Level::Root || is.level == Level::Unverified {
                        continue;
                    }
                    is
                }
            };
            let cur = st.get(&subject.key_id).copied().unwrap_or(Standing {
                level: Level::Unverified,
                depth: None,
            });
            if offer.level > Level::Unverified && rank(&offer) > rank(&cur) {
                st.insert(subject.key_id.clone(), offer);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    st
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn k(id: &str) -> KeyNode {
        KeyNode {
            key_id: id.into(),
            status: "active".into(),
            is_root: id == "root",
            level_cap: None,
            demoted_at: None,
        }
    }
    fn t(s: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + s, 0).unwrap()
    }
    fn v(i: &str, s: &str, l: Level) -> Edge {
        Edge {
            issuer: i.into(),
            subject: s.into(),
            granted: Some(l),
            received_at: t(0),
        }
    }

    #[test]
    fn grant_rules() {
        use Level::*;
        assert!(can_grant(Root, Verifier));
        assert!(!can_grant(Verifier, Verifier));
        assert!(can_grant(Verifier, Trusted));
        assert!(!can_grant(Trusted, Trusted));
        assert!(can_grant(Trusted, Member));
        assert!(!can_grant(Member, Member));
        assert!(!can_grant(Root, Root));
        assert_eq!(Verifier.demoted(), Trusted);
        assert_eq!(Member.demoted(), Member);
        assert_eq!(Root.demoted(), Root);
    }

    #[test]
    fn chain_and_depth_limit() {
        use Level::*;
        let keys: Vec<_> = ["root", "v", "t", "m", "x"].iter().map(|i| k(i)).collect();
        let edges = vec![
            v("root", "v", Verifier),
            v("v", "t", Trusted),
            v("t", "m", Member),
            v("t", "x", Member),
        ];
        let s = recompute(&keys, &edges, 4);
        assert_eq!(s["v"].level, Verifier);
        assert_eq!(s["t"].depth, Some(2));
        assert_eq!(s["m"].depth, Some(3));
        let s = recompute(&keys, &edges, 2);
        assert_eq!(s["m"].level, Unverified);
        // order independence
        let mut rev = edges.clone();
        rev.reverse();
        assert_eq!(recompute(&keys, &rev, 4), recompute(&keys, &edges, 4));
    }

    #[test]
    fn highest_vouch_wins_and_loss_cascades() {
        use Level::*;
        let keys: Vec<_> = ["root", "v", "t", "m"].iter().map(|i| k(i)).collect();
        let mut edges = vec![
            v("root", "v", Verifier),
            v("v", "t", Trusted),
            v("root", "t", Member),
            v("t", "m", Member),
        ];
        let s = recompute(&keys, &edges, 4);
        assert_eq!(s["t"].level, Trusted);
        assert_eq!(s["m"].level, Member);
        // drop v's vouch: t falls to member, m loses its chain (member can't vouch)
        edges.remove(0);
        let s = recompute(&keys, &edges, 4);
        assert_eq!(s["v"].level, Unverified);
        assert_eq!(s["t"].level, Member);
        assert_eq!(s["m"].level, Unverified);
    }

    #[test]
    fn cap_and_rotate() {
        use Level::*;
        let mut keys: Vec<_> = ["root", "v", "n"].iter().map(|i| k(i)).collect();
        keys[1].level_cap = Some(Trusted);
        keys[1].demoted_at = Some(t(10));
        keys[1].status = "rotated".into();
        let edges = vec![
            v("root", "v", Verifier),
            Edge {
                issuer: "v".into(),
                subject: "n".into(),
                granted: None,
                received_at: t(20),
            },
        ];
        let s = recompute(&keys, &edges, 4);
        assert_eq!(s["v"].level, Trusted);
        assert_eq!(s["n"].level, Trusted);
        assert_eq!(s["n"].depth, Some(1));
        // a later vouch lifts the cap
        let mut e2 = edges.clone();
        e2.push(Edge {
            received_at: t(30),
            ..v("root", "v", Verifier)
        });
        assert_eq!(recompute(&keys, &e2, 4)["v"].level, Verifier);
    }
}
