//! Token accounting and per-key limits (virtual keys).
//!
//! An API key is a virtual key when its scopes carry `tpm:<n>` (tokens per
//! minute) or `budget:<n>` (lifetime tokens). Usage is charged from what
//! the engine reported, after each response. Limits are per-router; totals
//! persist to `[auth].usage_file` so restarts don't reset budgets.
//!
//! The check happens before dispatch using tokens already spent, so a
//! request in flight can overshoot by its own size, never by more.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::LazyLock;

use cgn_auth::Principal;
use cgn_telemetry::counter_vec;
use cgn_telemetry::prometheus::IntCounterVec;
use dashmap::DashMap;
use parking_lot::Mutex;
use serde::Serialize;

static TENANT_TOKENS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    counter_vec!(
        "cgn_router_tenant_tokens_total",
        "Tokens charged per tenant; kind is prompt or completion.",
        &["tenant", "kind"]
    )
});

static LIMITED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    counter_vec!(
        "cgn_router_token_limit_rejected_total",
        "Requests refused by a key's token limit; limit is tpm or budget.",
        &["limit"]
    )
});

const MINUTE_SECS: u64 = 60;

#[derive(Default)]
struct Entry {
    total: AtomicU64,
    /// (minute number, tokens charged in it).
    window: Mutex<(u64, u64)>,
}

/// A token limit was hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exceeded {
    pub limit: &'static str,
    pub message: String,
    pub retry_after_secs: u64,
}

/// What `GET /v1/usage` reports.
#[derive(Debug, Serialize)]
pub struct UsageReport {
    pub subject: String,
    pub tenant: String,
    pub total_tokens: u64,
    pub tokens_this_minute: u64,
    pub tpm_limit: Option<u64>,
    pub budget_limit: Option<u64>,
}

#[derive(Default)]
pub struct UsageLedger {
    entries: DashMap<String, Entry>,
    path: Option<PathBuf>,
}

fn now_secs() -> u64 {
    chrono::Utc::now().timestamp().max(0) as u64
}

impl UsageLedger {
    /// Open the ledger, restoring totals from `path`. An unreadable file
    /// is an error at the call site's discretion: it would reset budgets.
    pub fn open(path: Option<PathBuf>) -> std::io::Result<Self> {
        let ledger = Self {
            entries: DashMap::new(),
            path,
        };
        if let Some(p) = ledger.path.as_ref().filter(|p| p.exists()) {
            let saved: BTreeMap<String, u64> = serde_json::from_slice(&std::fs::read(p)?)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            for (subject, total) in saved {
                ledger.entries.insert(
                    subject,
                    Entry {
                        total: AtomicU64::new(total),
                        ..Entry::default()
                    },
                );
            }
        }
        Ok(ledger)
    }

    /// Refuse the request if `p` has spent its budget or this minute's
    /// allowance.
    pub fn check(&self, p: &Principal) -> Result<(), Exceeded> {
        self.check_at(p, now_secs())
    }

    fn check_at(&self, p: &Principal, now: u64) -> Result<(), Exceeded> {
        let (tpm, budget) = (p.limit("tpm"), p.limit("budget"));
        if tpm.is_none() && budget.is_none() {
            return Ok(());
        }
        let Some(e) = self.entries.get(&p.subject) else {
            return Ok(());
        };
        if let Some(budget) = budget {
            if e.total.load(Ordering::Relaxed) >= budget {
                LIMITED.with_label_values(&["budget"]).inc();
                return Err(Exceeded {
                    limit: "budget",
                    message: format!("token budget of {budget} exhausted"),
                    retry_after_secs: 0,
                });
            }
        }
        if let Some(tpm) = tpm {
            let w = e.window.lock();
            if w.0 == now / MINUTE_SECS && w.1 >= tpm {
                LIMITED.with_label_values(&["tpm"]).inc();
                return Err(Exceeded {
                    limit: "tpm",
                    message: format!("rate of {tpm} tokens per minute exceeded"),
                    retry_after_secs: MINUTE_SECS - now % MINUTE_SECS,
                });
            }
        }
        Ok(())
    }

    /// Record tokens against `subject`.
    pub fn charge(&self, subject: &str, tenant: &str, prompt: u32, completion: u32) {
        self.charge_at(subject, tenant, prompt, completion, now_secs());
    }

    fn charge_at(&self, subject: &str, tenant: &str, prompt: u32, completion: u32, now: u64) {
        let tokens = prompt as u64 + completion as u64;
        let e = self.entries.entry(subject.to_string()).or_default();
        e.total.fetch_add(tokens, Ordering::Relaxed);
        let mut w = e.window.lock();
        let minute = now / MINUTE_SECS;
        if w.0 != minute {
            *w = (minute, 0);
        }
        w.1 += tokens;
        drop(w);
        TENANT_TOKENS
            .with_label_values(&[tenant, "prompt"])
            .inc_by(prompt as u64);
        TENANT_TOKENS
            .with_label_values(&[tenant, "completion"])
            .inc_by(completion as u64);
    }

    pub fn report(&self, p: &Principal) -> UsageReport {
        let minute = now_secs() / MINUTE_SECS;
        let (total, this_minute) = self
            .entries
            .get(&p.subject)
            .map(|e| {
                let w = e.window.lock();
                (
                    e.total.load(Ordering::Relaxed),
                    if w.0 == minute { w.1 } else { 0 },
                )
            })
            .unwrap_or((0, 0));
        UsageReport {
            subject: p.subject.clone(),
            tenant: p.tenant(),
            total_tokens: total,
            tokens_this_minute: this_minute,
            tpm_limit: p.limit("tpm"),
            budget_limit: p.limit("budget"),
        }
    }

    /// Write totals to `usage_file` atomically. No-op without one.
    pub fn persist(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let snapshot: BTreeMap<String, u64> = self
            .entries
            .iter()
            .map(|e| (e.key().clone(), e.value().total.load(Ordering::Relaxed)))
            .collect();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&snapshot)?)?;
        std::fs::rename(tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cgn_auth::PrincipalKind;

    fn key(scopes: &[&str]) -> Principal {
        Principal {
            subject: "apikey:abc".into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            kind: PrincipalKind::ApiKey,
        }
    }

    #[test]
    fn unlimited_keys_are_never_refused() {
        let l = UsageLedger::default();
        l.charge("apikey:abc", "t", 1_000_000, 0);
        assert!(l.check(&key(&["chat"])).is_ok());
    }

    #[test]
    fn budget_is_lifetime() {
        let l = UsageLedger::default();
        let k = key(&["budget:100"]);
        l.charge_at("apikey:abc", "t", 60, 39, 0);
        assert!(l.check_at(&k, 0).is_ok());
        l.charge_at("apikey:abc", "t", 1, 0, 0);
        let e = l.check_at(&k, 100_000).unwrap_err();
        assert_eq!(e.limit, "budget");
    }

    #[test]
    fn tpm_resets_each_minute_and_says_when() {
        let l = UsageLedger::default();
        let k = key(&["tpm:100"]);
        l.charge_at("apikey:abc", "t", 100, 0, 610);
        let e = l.check_at(&k, 615).unwrap_err();
        assert_eq!((e.limit, e.retry_after_secs), ("tpm", 45));
        assert!(l.check_at(&k, 660).is_ok());
        l.charge_at("apikey:abc", "t", 1, 0, 660);
        assert!(l.check_at(&k, 661).is_ok(), "window must have reset");
    }

    #[test]
    fn totals_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let l = UsageLedger::open(Some(path.clone())).unwrap();
        l.charge("apikey:abc", "t", 70, 30);
        l.persist().unwrap();
        let reopened = UsageLedger::open(Some(path)).unwrap();
        assert!(reopened.check(&key(&["budget:100"])).is_err());
        assert_eq!(reopened.report(&key(&[])).total_tokens, 100);
    }

    #[test]
    fn corrupt_usage_file_is_an_error_not_a_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        std::fs::write(&path, b"not json").unwrap();
        assert!(UsageLedger::open(Some(path)).is_err());
    }
}
