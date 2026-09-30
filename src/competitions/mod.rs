//! Leaderboard and trading competition service with sybil resistance.
//!
//! Admins define competitions (time window, eligible underlyings, scoring
//! metric). Wallets opt in, and the public leaderboard is served from a
//! periodically recomputed snapshot table. Basic sybil heuristics flag
//! suspicious wallet clusters; flagged entries are hidden from the public
//! board until an admin reviews them.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// Scoring metric used to rank competition entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoringMetric {
    /// Return on invested capital.
    Roi,
    /// Absolute profit and loss.
    AbsolutePnl,
    /// Risk-adjusted return (return divided by volatility).
    RiskAdjustedReturn,
}

/// A competition definition created by an admin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Competition {
    pub id: i64,
    pub name: String,
    /// Inclusive start of the competition window (unix seconds).
    pub starts_at: i64,
    /// Exclusive end of the competition window (unix seconds).
    pub ends_at: i64,
    /// Underlyings eligible for scoring in this competition.
    pub eligible_underlyings: Vec<String>,
    pub metric: ScoringMetric,
}

/// A wallet's opt-in to a competition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitionEntry {
    pub competition_id: i64,
    pub wallet: String,
    /// When the wallet opted in (unix seconds); used for deterministic ties.
    pub entered_at: i64,
    /// Sybil flag; flagged entries are hidden from the public board.
    pub flagged: bool,
    /// Admin review outcome once a flagged entry has been reviewed.
    pub reviewed: bool,
}

/// A snapshotted, ranked score for a competition entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompetitionScore {
    pub competition_id: i64,
    pub wallet: String,
    pub score: f64,
    pub rank: i64,
    /// Snapshot generation time (unix seconds).
    pub computed_at: i64,
}

/// A position opened inside a competition window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub wallet: String,
    pub underlying: String,
    /// When the position was opened (unix seconds).
    pub opened_at: i64,
    /// Realized P&L for positions closed inside the window.
    pub realized_pnl: f64,
    /// Mark-to-market P&L for positions still open at the window end.
    pub unrealized_pnl: f64,
    /// Capital deployed, used for ROI and risk-adjusted scoring.
    pub capital: f64,
    /// Realized volatility of the position, used for risk-adjusted scoring.
    pub volatility: f64,
}

/// Session metadata captured at auth verify time, used for sybil heuristics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub wallet: String,
    pub ip: String,
    /// When the wallet/session was first seen (unix seconds).
    pub created_at: i64,
}

/// Sybil heuristic thresholds.
#[derive(Debug, Clone, Copy)]
pub struct SybilConfig {
    /// Number of distinct wallets sharing an IP before it is suspicious.
    pub shared_ip_wallet_threshold: usize,
    /// Window (seconds) within which wallet creation is considered a burst.
    pub burst_window_secs: i64,
    /// Number of wallets created in a burst before it is suspicious.
    pub burst_wallet_threshold: usize,
}

impl Default for SybilConfig {
    fn default() -> Self {
        Self {
            shared_ip_wallet_threshold: 3,
            burst_window_secs: 3600,
            burst_wallet_threshold: 5,
        }
    }
}

/// Compute the raw score for a single position under the given metric.
///
/// Only positions opened inside the window are counted; positions still open
/// at the end of the window are marked to market via `unrealized_pnl`.
fn position_score(metric: ScoringMetric, position: &Position) -> f64 {
    let total_pnl = position.realized_pnl + position.unrealized_pnl;
    match metric {
        ScoringMetric::AbsolutePnl => total_pnl,
        ScoringMetric::Roi => {
            if position.capital <= 0.0 {
                0.0
            } else {
                total_pnl / position.capital
            }
        }
        ScoringMetric::RiskAdjustedReturn => {
            if position.capital <= 0.0 || position.volatility <= 0.0 {
                0.0
            } else {
                (total_pnl / position.capital) / position.volatility
            }
        }
    }
}

/// Aggregate scores for every opted-in wallet, counting only positions opened
/// inside the competition window and on eligible underlyings.
fn aggregate_scores(
    competition: &Competition,
    entries: &[CompetitionEntry],
    positions: &[Position],
) -> HashMap<String, f64> {
    let eligible: HashSet<&str> = competition
        .eligible_underlyings
        .iter()
        .map(String::as_str)
        .collect();

    let mut scores: HashMap<String, f64> = entries
        .iter()
        .map(|entry| (entry.wallet.clone(), 0.0))
        .collect();

    for position in positions {
        if position.opened_at < competition.starts_at || position.opened_at >= competition.ends_at {
            continue;
        }
        if !eligible.contains(position.underlying.as_str()) {
            continue;
        }
        if let Some(score) = scores.get_mut(&position.wallet) {
            *score += position_score(competition.metric, position);
        }
    }

    scores
}

/// Flag wallets that share session IPs above the threshold or that were
/// created in coordinated bursts.
fn detect_sybil(
    entries: &[CompetitionEntry],
    sessions: &[SessionRecord],
    config: &SybilConfig,
) -> HashSet<String> {
    let mut flagged: HashSet<String> = HashSet::new();

    // Heuristic 1: wallets sharing an IP above the threshold.
    let mut wallets_by_ip: HashMap<&str, HashSet<&str>> = HashMap::new();
    for session in sessions {
        wallets_by_ip
            .entry(session.ip.as_str())
            .or_default()
            .insert(session.wallet.as_str());
    }
    for wallets in wallets_by_ip.values() {
        if wallets.len() >= config.shared_ip_wallet_threshold {
            for wallet in wallets {
                flagged.insert((*wallet).to_string());
            }
        }
    }

    // Heuristic 2: wallets created in coordinated bursts.
    let mut created: Vec<&SessionRecord> = sessions.iter().collect();
    created.sort_by_key(|session| session.created_at);
    let mut start = 0usize;
    for end in 0..created.len() {
        while created[end].created_at - created[start].created_at > config.burst_window_secs {
            start += 1;
        }
        if end - start + 1 >= config.burst_wallet_threshold {
            for session in &created[start..=end] {
                flagged.insert(session.wallet.clone());
            }
        }
    }

    // Only entries that actually opted in can be flagged.
    let opted_in: HashSet<&str> = entries.iter().map(|entry| entry.wallet.as_str()).collect();
    flagged.retain(|wallet| opted_in.contains(wallet.as_str()));
    flagged
}

/// Recompute the leaderboard snapshot for a competition.
///
/// Flagged entries are excluded from the public board until an admin reviews
/// them. Ties are broken deterministically: earlier entry wins.
fn recompute_snapshot(
    competition: &Competition,
    entries: &[CompetitionEntry],
    positions: &[Position],
    sessions: &[SessionRecord],
    config: &SybilConfig,
    computed_at: i64,
) -> Vec<CompetitionScore> {
    let flagged = detect_sybil(entries, sessions, config);
    let scores = aggregate_scores(competition, entries, positions);

    let mut ranked: Vec<(&CompetitionEntry, f64)> = entries
        .iter()
        .filter(|entry| !flagged.contains(&entry.wallet) || entry.reviewed)
        .map(|entry| {
            let score = scores.get(&entry.wallet).copied().unwrap_or(0.0);
            (entry, score)
        })
        .collect();

    // Deterministic ordering: higher score first, then earlier entry wins.
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.entered_at.cmp(&b.0.entered_at))
            .then_with(|| a.0.wallet.cmp(&b.0.wallet))
    });

    ranked
        .into_iter()
        .enumerate()
        .map(|(index, (entry, score))| CompetitionScore {
            competition_id: competition.id,
            wallet: entry.wallet.clone(),
            score,
            rank: index as i64 + 1,
            computed_at,
        })
        .collect()
}

/// Truncate a wallet address for privacy-preserving public display.
fn truncate_wallet(wallet: &str) -> String {
    if wallet.len() <= 10 {
        return wallet.to_string();
    }
    format!("{}...{}", &wallet[..6], &wallet[wallet.len() - 4..])
}

/// A single row of the public leaderboard response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaderboardRow {
    pub rank: i64,
    pub wallet: String,
    pub score: f64,
}

/// Build the public leaderboard response from a snapshot, truncating wallet
/// addresses by default.
fn public_leaderboard(snapshot: &[CompetitionScore]) -> Vec<LeaderboardRow> {
    snapshot
        .iter()
        .map(|score| LeaderboardRow {
            rank: score.rank,
            wallet: truncate_wallet(&score.wallet),
            score: score.score,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn competition(metric: ScoringMetric) -> Competition {
        Competition {
            id: 1,
            name: "Q1 Options Cup".to_string(),
            starts_at: 1_000,
            ends_at: 2_000,
            eligible_underlyings: vec!["BTC".to_string(), "ETH".to_string()],
            metric,
        }
    }

    fn entry(wallet: &str, entered_at: i64) -> CompetitionEntry {
        CompetitionEntry {
            competition_id: 1,
            wallet: wallet.to_string(),
            entered_at,
            flagged: false,
            reviewed: false,
        }
    }

    fn position(wallet: &str, opened_at: i64, realized: f64, unrealized: f64) -> Position {
        Position {
            wallet: wallet.to_string(),
            underlying: "BTC".to_string(),
            opened_at,
            realized_pnl: realized,
            unrealized_pnl: unrealized,
            capital: 1_000.0,
            volatility: 0.5,
        }
    }

    #[test]
    fn scoring_golden_roi() {
        let comp = competition(ScoringMetric::Roi);
        let entries = vec![entry("alice", 1_100), entry("bob", 1_200)];
        let positions = vec![
            position("alice", 1_100, 100.0, 0.0),
            position("bob", 1_200, 50.0, 0.0),
        ];
        let snapshot = recompute_snapshot(&comp, &entries, &positions, &[], &SybilConfig::default(), 2_100);
        assert_eq!(snapshot[0].wallet, "alice");
        assert!((snapshot[0].score - 0.1).abs() < 1e-9);
        assert_eq!(snapshot[1].wallet, "bob");
        assert!((snapshot[1].score - 0.05).abs() < 1e-9);
    }

    #[test]
    fn window_boundary_excludes_outside_positions() {
        let comp = competition(ScoringMetric::AbsolutePnl);
        let entries = vec![entry("alice", 1_000)];
        let positions = vec![
            position("alice", 999, 1_000.0, 0.0),   // before window
            position("alice", 2_000, 1_000.0, 0.0), // at exclusive end
            position("alice", 1_500, 25.0, 25.0),   // inside, marked to market
        ];
        let snapshot = recompute_snapshot(&comp, &entries, &positions, &[], &SybilConfig::default(), 2_100);
        assert!((snapshot[0].score - 50.0).abs() < 1e-9);
    }

    #[test]
    fn ties_broken_by_earlier_entry() {
        let comp = competition(ScoringMetric::AbsolutePnl);
        let entries = vec![entry("late", 1_500), entry("early", 1_100)];
        let positions = vec![
            position("late", 1_500, 10.0, 0.0),
            position("early", 1_100, 10.0, 0.0),
        ];
        let snapshot = recompute_snapshot(&comp, &entries, &positions, &[], &SybilConfig::default(), 2_100);
        assert_eq!(snapshot[0].wallet, "early");
        assert_eq!(snapshot[1].wallet, "late");
    }

    #[test]
    fn sybil_flagged_entries_hidden_until_reviewed() {
        let comp = competition(ScoringMetric::AbsolutePnl);
        let entries = vec![entry("alice", 1_100), entry("bob", 1_200), entry("carol", 1_300)];
        let positions = vec![
            position("alice", 1_100, 10.0, 0.0),
            position("bob", 1_200, 20.0, 0.0),
            position("carol", 1_300, 30.0, 0.0),
        ];
        let sessions = vec![
            SessionRecord { wallet: "alice".to_string(), ip: "1.2.3.4".to_string(), created_at: 100 },
            SessionRecord { wallet: "bob".to_string(), ip: "1.2.3.4".to_string(), created_at: 110 },
            SessionRecord { wallet: "carol".to_string(), ip: "1.2.3.4".to_string(), created_at: 120 },
        ];
        let snapshot = recompute_snapshot(&comp, &entries, &positions, &sessions, &SybilConfig::default(), 2_100);
        assert!(snapshot.is_empty());

        let mut reviewed = entries.clone();
        reviewed[2].reviewed = true;
        let snapshot = recompute_snapshot(&comp, &reviewed, &positions, &sessions, &SybilConfig::default(), 2_100);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].wallet, "carol");
    }

    #[test]
    fn public_leaderboard_truncates_wallets() {
        let snapshot = vec![CompetitionScore {
            competition_id: 1,
            wallet: "0x1234567890abcdef1234".to_string(),
            score: 1.0,
            rank: 1,
            computed_at: 2_100,
        }];
        let board = public_leaderboard(&snapshot);
        assert_eq!(board[0].wallet, "0x1234...1234");
    }
}
