//! Account selection within one provider. Unknown quotas never become invented limits.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    RoundRobin,
    FillFirst,
    ResetFirst,
    #[default]
    Smart,
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub id: String,
    pub remaining_percent: Option<f64>,
    pub weekly_reset: Option<i64>,
}

#[derive(Default)]
pub struct Scheduler {
    positions: HashMap<String, usize>,
    credits: HashMap<(String, String), f64>,
}

impl Scheduler {
    pub fn order(
        &mut self,
        provider: &str,
        strategy: Strategy,
        candidates: &[Candidate],
        now: i64,
    ) -> Vec<String> {
        let mut available: Vec<&Candidate> = candidates
            .iter()
            .filter(|a| {
                a.remaining_percent.is_none_or(|n| n > 0.0)
                    || a.weekly_reset.is_some_and(|t| t <= now)
            })
            .collect();
        available.sort_by(|a, b| a.id.cmp(&b.id));
        if available.is_empty() {
            return vec![];
        }
        match strategy {
            Strategy::FillFirst => {}
            Strategy::RoundRobin => {
                let cursor = self.positions.entry(provider.to_owned()).or_default();
                let offset = *cursor % available.len();
                available.rotate_left(offset);
                *cursor = cursor.wrapping_add(1);
            }
            Strategy::ResetFirst => {
                available.sort_by_key(|a| a.weekly_reset.filter(|t| *t > now).unwrap_or(i64::MAX))
            }
            Strategy::Smart => {
                // The nearest reset owns 90% of new assignments during its last
                // 48 hours, including its final allowance. Outside that window,
                // use quota/time weighting with 20% equal rotation.
                let draining = available
                    .iter()
                    .filter_map(|a| {
                        a.weekly_reset
                            .filter(|t| *t > now && t.saturating_sub(now) < 48 * 3600)
                            .map(|reset| (reset, a.id.as_str()))
                    })
                    .min()
                    .map(|(_, id)| id);
                let raw: Vec<f64> = available
                    .iter()
                    .map(|a| {
                        let hours = a
                            .weekly_reset
                            .filter(|t| *t > now)
                            .map(|t| ((t - now) as f64 / 3600.0).max(1.0))
                            .unwrap_or(168.0);
                        a.remaining_percent.unwrap_or(50.0).clamp(1.0, 100.0) / hours
                    })
                    .collect();
                let total: f64 = raw.iter().sum();
                let floor = 0.2 / available.len() as f64;
                let mut scores = HashMap::new();
                self.credits
                    .retain(|(p, id), _| p != provider || available.iter().any(|a| a.id == *id));
                for (account, weight) in available.iter().zip(raw) {
                    let credit = self
                        .credits
                        .entry((provider.to_owned(), account.id.clone()))
                        .or_default();
                    *credit += match draining {
                        Some(_) if available.len() == 1 => 1.0,
                        Some(id) if id == account.id => 0.9,
                        Some(_) => 0.1 / (available.len() - 1) as f64,
                        None => 0.8 * weight / total + floor,
                    };
                    scores.insert(account.id.clone(), *credit);
                }
                available.sort_by(|a, b| {
                    scores[&b.id]
                        .total_cmp(&scores[&a.id])
                        .then_with(|| a.id.cmp(&b.id))
                });
                if let Some(credit) = self
                    .credits
                    .get_mut(&(provider.to_owned(), available[0].id.clone()))
                {
                    *credit -= 1.0;
                }
            }
        }
        available.into_iter().map(|a| a.id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn account(id: &str, remaining: Option<f64>, reset: Option<i64>) -> Candidate {
        Candidate {
            id: id.into(),
            remaining_percent: remaining,
            weekly_reset: reset,
        }
    }
    #[test]
    fn rotation_and_fill_first_are_distinct() {
        let mut s = Scheduler::default();
        let a = vec![account("a", None, None), account("b", None, None)];
        assert_eq!(
            s.order("claude", Strategy::RoundRobin, &a, 0),
            vec!["a", "b"]
        );
        assert_eq!(
            s.order("claude", Strategy::RoundRobin, &a, 0),
            vec!["b", "a"]
        );
        assert_eq!(
            s.order("claude", Strategy::FillFirst, &a, 0),
            vec!["a", "b"]
        );
    }
    #[test]
    fn reset_first_uses_weekly_expiry_and_skips_empty_accounts() {
        let a = vec![
            account("later", Some(90.0), Some(900)),
            account("soon", Some(30.0), Some(300)),
            account("empty", Some(0.0), Some(100)),
        ];
        assert_eq!(
            Scheduler::default().order("claude", Strategy::ResetFirst, &a, 0),
            vec!["soon", "later"]
        );
    }
    #[test]
    fn smart_consumes_every_account_and_favors_the_next_reset() {
        let mut s = Scheduler::default();
        let a = vec![
            account("soon", Some(100.0), Some(86400)),
            account("later", Some(100.0), Some(6 * 86400)),
        ];
        let count = (0..100)
            .filter(|_| s.order("claude", Strategy::Smart, &a, 0)[0] == "soon")
            .count();
        assert_eq!(count, 90, "next reset must receive 90/100 assignments");
    }
    #[test]
    fn smart_drains_the_last_percent_before_an_imminent_reset() {
        let mut s = Scheduler::default();
        let a = vec![
            account("soon", Some(1.0), Some(86400)),
            account("later", Some(100.0), Some(6 * 86400)),
        ];
        let later = (0..100)
            .filter(|_| s.order("claude", Strategy::Smart, &a, 0)[0] == "later")
            .count();
        assert_eq!(later, 10, "the final percent must keep its 90% priority");
    }
    #[test]
    fn smart_moves_to_the_next_reset_only_after_exhaustion() {
        let mut s = Scheduler::default();
        let a = vec![
            account("first", Some(0.0), Some(3600)),
            account("second", Some(2.0), Some(86400)),
            account("later", Some(100.0), Some(6 * 86400)),
        ];
        let mut second = 0;
        for _ in 0..100 {
            let order = s.order("claude", Strategy::Smart, &a, 0);
            assert!(!order.iter().any(|id| id == "first"));
            second += usize::from(order[0] == "second");
        }
        assert_eq!(second, 90);
    }
    #[test]
    fn smart_selects_the_earliest_of_several_imminent_resets() {
        let mut s = Scheduler::default();
        let a = vec![
            account("first", Some(2.0), Some(3600)),
            account("second", Some(100.0), Some(86400)),
            account("later", Some(100.0), Some(6 * 86400)),
        ];
        let mut counts = HashMap::new();
        for _ in 0..1000 {
            *counts
                .entry(s.order("claude", Strategy::Smart, &a, 0)[0].clone())
                .or_insert(0) += 1;
        }
        assert_eq!(counts["first"], 900);
        assert!(counts["second"] > 0 && counts["later"] > 0);
    }
    #[test]
    fn smart_enters_the_drain_window_as_time_advances() {
        let mut s = Scheduler::default();
        let a = vec![
            account("soon", Some(2.0), Some(3 * 86400)),
            account("later", Some(100.0), Some(6 * 86400)),
        ];
        let before = (0..100)
            .filter(|_| s.order("claude", Strategy::Smart, &a, 0)[0] == "soon")
            .count();
        let after = (0..100)
            .filter(|_| s.order("claude", Strategy::Smart, &a, 86401)[0] == "soon")
            .count();
        assert!(before < 20);
        assert!((89..=91).contains(&after));
    }
    #[test]
    fn unknown_quotas_rotate_without_fabricating_exhaustion() {
        let mut s = Scheduler::default();
        let a = vec![account("a", None, None), account("b", None, None)];
        let count = (0..100)
            .filter(|_| s.order("codex", Strategy::Smart, &a, 0)[0] == "a")
            .count();
        assert_eq!(count, 50);
    }
    #[test]
    fn expired_zero_quota_can_be_retried_after_reset() {
        let a = vec![account("a", Some(0.0), Some(10))];
        assert_eq!(
            Scheduler::default().order("codex", Strategy::Smart, &a, 11),
            vec!["a"]
        );
    }
}
