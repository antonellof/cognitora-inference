//! Multi-model cascade (SLM → Mid → LLM).
//!
//! When `[router.cascade] enabled = true` and the request's model has a
//! cascade list configured (or one passed in `GenerateRequest.cascade`),
//! the router runs the request against the smallest model first. If the
//! response's confidence (mean logprob) drops below
//! `confidence_threshold`, the next model in the cascade is tried.
//!
//! Today this module exposes the FSM and selector logic; the gateway
//! invokes it as a wrapper around `routing::pick`.

use cgn_core::config::Config;

#[derive(Debug, Clone)]
pub struct Cascade {
    pub steps: Vec<String>,
    pub threshold: f32,
}

impl Cascade {
    pub fn from_config(cfg: &Config, model: &str, override_chain: &[String]) -> Option<Self> {
        if !cfg.router.cascade.enabled { return None; }
        let chain = if !override_chain.is_empty() {
            override_chain.to_vec()
        } else {
            cfg.models.get(model)?.cascade.clone()
        };
        if chain.is_empty() { return None; }
        Some(Self {
            steps: chain,
            threshold: cfg.router.cascade.confidence_threshold,
        })
    }

    /// Decide whether to escalate after observing `logprob_mean`.
    pub fn should_escalate(&self, logprob_mean: f32) -> bool {
        logprob_mean < self.threshold
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
        let casc = Cascade { steps: vec![], threshold: -1.0 };
        assert!(casc.should_escalate(-1.5));
        assert!(!casc.should_escalate(-0.5));
    }
}
