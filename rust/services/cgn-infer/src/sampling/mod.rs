//! Logits processing: temperature, top-k, top-p, repetition penalty.
//!
//! The sampler is deliberately independent of Candle — it operates on
//! a plain `&mut [f32]` logits slice so it can be unit-tested without
//! a model and reused unchanged when the runtime backend is swapped.

use rand::distributions::Distribution;
use rand::rngs::StdRng;
use rand::SeedableRng;

/// User-controllable sampling knobs, mirroring the OpenAI request
/// fields plus the llama.cpp-style extensions (top-k, repetition
/// penalty) the platform's recipes already use.
#[derive(Debug, Clone)]
pub struct SamplingParams {
    /// 0.0 (or below `GREEDY_EPS`) means greedy/argmax decoding.
    pub temperature: f32,
    /// Nucleus sampling threshold in (0, 1]; 1.0 disables it.
    pub top_p: f32,
    /// Keep only the k most likely tokens; 0 disables it.
    pub top_k: usize,
    /// Multiplicative penalty (>1 discourages repeats); 1.0 disables.
    pub repetition_penalty: f32,
    /// How many recent tokens the repetition penalty looks at.
    pub repeat_last_n: usize,
    /// Optional seed for reproducible sampling.
    pub seed: Option<u64>,
}

impl Default for SamplingParams {
    fn default() -> Self {
        Self {
            temperature: 1.0,
            top_p: 1.0,
            top_k: 0,
            repetition_penalty: 1.0,
            repeat_last_n: 64,
            seed: None,
        }
    }
}

const GREEDY_EPS: f32 = 1e-4;

/// Stateful sampler: owns the RNG so repeated calls advance it.
pub struct Sampler {
    params: SamplingParams,
    rng: StdRng,
}

impl Sampler {
    pub fn new(params: SamplingParams) -> Self {
        let rng = match params.seed {
            Some(s) => StdRng::seed_from_u64(s),
            None => StdRng::from_entropy(),
        };
        Self { params, rng }
    }

    /// Pick the next token id from raw logits, given the tokens already
    /// generated (for the repetition penalty window).
    pub fn sample(&mut self, logits: &mut [f32], recent_tokens: &[u32]) -> u32 {
        apply_repetition_penalty(
            logits,
            recent_tokens,
            self.params.repetition_penalty,
            self.params.repeat_last_n,
        );

        if self.params.temperature < GREEDY_EPS {
            return argmax(logits);
        }

        for l in logits.iter_mut() {
            *l /= self.params.temperature;
        }

        // Work on (index, logit) pairs sorted by descending logit so
        // top-k and top-p are both simple prefix truncations.
        let mut ranked: Vec<(usize, f32)> =
            logits.iter().copied().enumerate().collect();
        ranked.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));

        if self.params.top_k > 0 && self.params.top_k < ranked.len() {
            ranked.truncate(self.params.top_k);
        }

        let mut probs = softmax(ranked.iter().map(|&(_, l)| l));

        if self.params.top_p < 1.0 {
            let mut cum = 0.0f32;
            let mut keep = probs.len();
            for (i, p) in probs.iter().enumerate() {
                cum += p;
                if cum >= self.params.top_p {
                    keep = i + 1;
                    break;
                }
            }
            ranked.truncate(keep);
            probs.truncate(keep);
            let sum: f32 = probs.iter().sum();
            for p in probs.iter_mut() {
                *p /= sum;
            }
        }

        let dist = rand::distributions::WeightedIndex::new(&probs)
            .expect("non-empty positive weights");
        let picked = dist.sample(&mut self.rng);
        ranked[picked].0 as u32
    }
}

/// llama.cpp-style penalty: divide positive logits, multiply negative
/// ones, for every distinct token in the recent window.
fn apply_repetition_penalty(
    logits: &mut [f32],
    recent: &[u32],
    penalty: f32,
    last_n: usize,
) {
    if (penalty - 1.0).abs() < f32::EPSILON || last_n == 0 {
        return;
    }
    let start = recent.len().saturating_sub(last_n);
    for &tok in &recent[start..] {
        if let Some(l) = logits.get_mut(tok as usize) {
            if *l > 0.0 {
                *l /= penalty;
            } else {
                *l *= penalty;
            }
        }
    }
}

fn argmax(logits: &[f32]) -> u32 {
    logits
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, _)| i as u32)
        .unwrap_or(0)
}

fn softmax(logits: impl Iterator<Item = f32> + Clone) -> Vec<f32> {
    let max = logits.clone().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = logits.map(|l| (l - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    exps.into_iter().map(|e| e / sum).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sampler(params: SamplingParams) -> Sampler {
        Sampler::new(SamplingParams {
            seed: Some(42),
            ..params
        })
    }

    #[test]
    fn greedy_picks_argmax() {
        let mut s = sampler(SamplingParams {
            temperature: 0.0,
            ..Default::default()
        });
        let mut logits = vec![0.1, 3.0, -1.0, 2.9];
        assert_eq!(s.sample(&mut logits, &[]), 1);
    }

    #[test]
    fn top_k_restricts_candidates() {
        let mut s = sampler(SamplingParams {
            temperature: 1.0,
            top_k: 2,
            ..Default::default()
        });
        // Tokens 1 and 3 dominate; with top_k=2 nothing else can win.
        for _ in 0..64 {
            let mut logits = vec![0.0, 10.0, 0.0, 9.0, 0.0];
            let t = s.sample(&mut logits, &[]);
            assert!(t == 1 || t == 3, "unexpected token {t}");
        }
    }

    #[test]
    fn top_p_collapses_to_dominant_token() {
        let mut s = sampler(SamplingParams {
            temperature: 1.0,
            top_p: 0.5,
            ..Default::default()
        });
        // Token 0 holds ~95% of the mass, so a 0.5 nucleus is just it.
        for _ in 0..32 {
            let mut logits = vec![10.0, 5.0, 4.0, 3.0];
            assert_eq!(s.sample(&mut logits, &[]), 0);
        }
    }

    #[test]
    fn repetition_penalty_discourages_repeats() {
        let mut s = sampler(SamplingParams {
            temperature: 0.0,
            repetition_penalty: 100.0,
            ..Default::default()
        });
        // Token 1 wins greedily, but a huge penalty on it (already
        // generated) flips the argmax to token 3.
        let mut logits = vec![0.1, 3.0, -1.0, 2.9];
        assert_eq!(s.sample(&mut logits, &[1]), 3);
    }

    #[test]
    fn repetition_penalty_respects_window() {
        let mut s = sampler(SamplingParams {
            temperature: 0.0,
            repetition_penalty: 100.0,
            repeat_last_n: 1,
            ..Default::default()
        });
        // Token 1 was generated, but outside the 1-token window
        // (only token 2 is inside), so it still wins.
        let mut logits = vec![0.1, 3.0, -1.0, 2.9];
        assert_eq!(s.sample(&mut logits, &[1, 2]), 1);
    }

    #[test]
    fn seeded_sampling_is_reproducible() {
        let params = SamplingParams {
            temperature: 1.0,
            ..Default::default()
        };
        let mut a = sampler(params.clone());
        let mut b = sampler(params);
        for _ in 0..16 {
            let mut la = vec![1.0, 2.0, 3.0, 2.5];
            let mut lb = la.clone();
            assert_eq!(a.sample(&mut la, &[]), b.sample(&mut lb, &[]));
        }
    }
}
