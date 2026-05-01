//! Multi-model cascade (SLM → Mid → LLM).
//!
//! When `[router.cascade] enabled = true` and the request's model has a
//! cascade list configured (or one passed in `GenerateRequest.cascade`),
//! the router runs the request against the smallest model first. If the
//! response's confidence (mean logprob) drops below
//! `confidence_threshold`, the next model in the cascade is tried.
//!
//! `Cascade::run` is an async orchestrator: it takes a closure that
//! executes a single step (model name) and returns the produced text +
//! mean logprob, and drives the chain until either confidence is met or
//! the chain is exhausted.

use std::future::Future;

use cgn_core::config::Config;

#[derive(Debug, Clone)]
pub struct Cascade {
    pub steps: Vec<String>,
    pub threshold: f32,
}

/// Single-step output the cascade evaluates.
#[derive(Debug, Clone, Default)]
pub struct StepOutcome {
    pub text: String,
    pub logprob: f32,
    /// Number of completion tokens — 0 means the engine did not return
    /// any usable output and the cascade should escalate.
    pub tokens: u32,
    pub finish: String,
}

#[derive(Debug, Clone)]
pub struct CascadeResult {
    pub outcome: StepOutcome,
    pub model_used: String,
    pub steps_attempted: Vec<String>,
}

impl Cascade {
    pub fn from_config(cfg: &Config, model: &str, override_chain: &[String]) -> Option<Self> {
        if !cfg.router.cascade.enabled {
            return None;
        }
        let chain = if !override_chain.is_empty() {
            override_chain.to_vec()
        } else {
            cfg.models.get(model)?.cascade.clone()
        };
        if chain.is_empty() {
            return None;
        }
        Some(Self {
            steps: chain,
            threshold: cfg.router.cascade.confidence_threshold,
        })
    }

    /// Decide whether to escalate after observing `outcome`.
    pub fn should_escalate(&self, outcome: &StepOutcome) -> bool {
        outcome.tokens == 0 || outcome.logprob < self.threshold
    }

    /// Drive the cascade. `step_fn` is invoked once per step with the
    /// model name; the first step whose confidence ≥ threshold wins.
    /// If every step escalates, the last outcome is returned anyway —
    /// the caller can log it as a low-confidence answer.
    pub async fn run<F, Fut>(&self, mut step_fn: F) -> CascadeResult
    where
        F: FnMut(&str) -> Fut,
        Fut: Future<Output = StepOutcome>,
    {
        let mut last = StepOutcome::default();
        let mut last_model = String::new();
        let mut attempted = Vec::with_capacity(self.steps.len());
        for step in &self.steps {
            attempted.push(step.clone());
            let outcome = step_fn(step.as_str()).await;
            last_model = step.clone();
            last = outcome;
            if !self.should_escalate(&last) {
                break;
            }
            tracing::debug!(
                step = %last_model,
                logprob = last.logprob,
                threshold = self.threshold,
                "cascade escalating"
            );
        }
        CascadeResult {
            outcome: last,
            model_used: last_model,
            steps_attempted: attempted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(steps: Vec<&str>, enabled: bool) -> Config {
        let mut c = Config::default();
        c.router.cascade.enabled = enabled;
        c.router.cascade.confidence_threshold = -1.0;
        c.models.insert(
            "m".into(),
            cgn_core::config::ModelConfig {
                cascade: steps.into_iter().map(String::from).collect(),
                ..Default::default()
            },
        );
        c
    }

    #[test]
    fn disabled_returns_none() {
        let c = cfg_with(vec!["s", "m", "l"], false);
        assert!(Cascade::from_config(&c, "m", &[]).is_none());
    }

    #[test]
    fn override_takes_precedence() {
        let c = cfg_with(vec!["a"], true);
        let casc = Cascade::from_config(&c, "m", &["x".into(), "y".into()]).unwrap();
        assert_eq!(casc.steps, vec!["x", "y"]);
    }

    #[test]
    fn escalates_below_threshold() {
        let casc = Cascade {
            steps: vec![],
            threshold: -1.0,
        };
        let low = StepOutcome {
            tokens: 5,
            logprob: -1.5,
            ..Default::default()
        };
        let hi = StepOutcome {
            tokens: 5,
            logprob: -0.5,
            ..Default::default()
        };
        assert!(casc.should_escalate(&low));
        assert!(!casc.should_escalate(&hi));
    }

    #[tokio::test]
    async fn run_short_circuits_on_high_confidence() {
        let casc = Cascade {
            steps: vec!["s".into(), "m".into(), "l".into()],
            threshold: -1.0,
        };
        use std::sync::atomic::{AtomicU32, Ordering};
        let calls = AtomicU32::new(0);
        let r = casc
            .run(|step| {
                calls.fetch_add(1, Ordering::SeqCst);
                let s = step.to_string();
                async move {
                    StepOutcome {
                        text: s.clone(),
                        tokens: 3,
                        logprob: -0.1,
                        finish: "stop".into(),
                    }
                }
            })
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(r.model_used, "s");
    }

    #[tokio::test]
    async fn run_escalates_through_chain() {
        let casc = Cascade {
            steps: vec!["s".into(), "m".into(), "l".into()],
            threshold: -1.0,
        };
        use std::sync::atomic::{AtomicU32, Ordering};
        let calls = AtomicU32::new(0);
        let r = casc
            .run(|step| {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                let s = step.to_string();
                async move {
                    let lp = if n < 2 { -2.0 } else { -0.1 };
                    StepOutcome {
                        text: s.clone(),
                        tokens: 3,
                        logprob: lp,
                        finish: "stop".into(),
                    }
                }
            })
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(r.model_used, "l");
        assert_eq!(
            r.steps_attempted,
            vec!["s".to_string(), "m".into(), "l".into()]
        );
    }
}
