//! Continuous-batching scheduler (phase 3).
//!
//! Replaces the phase-1 "async mutex around the model" with a
//! dedicated engine thread that multiplexes many sequences over one
//! [`BatchModel`]:
//!
//! * **Admission** — waiting requests join the running set while
//!   there is batch room and KV-pool space for their prompt.
//! * **Chunked prefill** — long prompts are processed
//!   `prefill_chunk` tokens at a time, one chunk per scheduler step,
//!   so a long prompt cannot starve decoding sequences.
//! * **Batched decode** — all running decode-phase sequences advance
//!   one token per step through `BatchModel::decode`. With the
//!   [`crate::runtime::BatchedLlama`] runtime the weight matmuls run
//!   once for the whole batch; with `max_batch() == 1` runtimes
//!   (sequential fallback, pipeline) the scheduler degrades to
//!   one-at-a-time serving automatically.
//! * **Paged KV with preemption** — KV space is accounted in
//!   [`crate::kv::BLOCK_TOKENS`]-sized blocks against a fixed pool
//!   ([`crate::kv::KvPool`]). When a growing sequence cannot get a
//!   block, the youngest running sequence is preempted: its blocks
//!   and runtime KV are dropped and it is re-queued for a fresh
//!   prefill (its already-generated tokens are kept, so no output is
//!   lost).
//!
//! The scheduler is deliberately runtime-agnostic and is unit-tested
//! against a mock [`BatchModel`].

use std::collections::VecDeque;
use std::sync::mpsc as std_mpsc;

use cgn_core::{Error, Result};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::engine::StreamEvent;
use crate::kv::KvPool;
use crate::runtime::BatchModel;
use crate::sampling::{Sampler, SamplingParams};

/// Scheduler tuning knobs.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Max sequences decoding concurrently (clamped to the runtime's
    /// `max_batch`).
    pub max_batch: usize,
    /// Prefill chunk size in tokens.
    pub prefill_chunk: usize,
    /// Total KV pool size in tokens (converted to blocks).
    pub kv_pool_tokens: usize,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_batch: 8,
            prefill_chunk: 512,
            kv_pool_tokens: 64 * 1024,
        }
    }
}

/// What the HTTP layer submits.
pub struct Request {
    pub prompt_tokens: Vec<u32>,
    pub max_tokens: usize,
    pub stop: Vec<String>,
    pub params: SamplingParams,
    /// Detokenizer for incremental output.
    pub tokenizer: tokenizers::Tokenizer,
    pub eos_token_id: Option<u32>,
    pub tx: mpsc::Sender<StreamEvent>,
}

enum Phase {
    /// `done` prompt tokens are already in the runtime's KV state.
    Prefill {
        done: usize,
    },
    Decode,
}

struct Sequence {
    id: u64,
    req: Request,
    phase: Phase,
    /// Prompt + generated tokens.
    tokens: Vec<u32>,
    prompt_len: usize,
    generated: Vec<u32>,
    sampler: Sampler,
    /// Bytes of decoded text already emitted.
    emitted_len: usize,
    /// Admission order tick, used to pick preemption victims (LIFO).
    admitted_at: u64,
}

impl Sequence {
    fn budget(&self, max_seq_len: usize) -> usize {
        self.req
            .max_tokens
            .min(max_seq_len.saturating_sub(self.prompt_len))
    }
}

/// Handle used by the HTTP layer. Cloneable; requests are queued to
/// the engine thread.
#[derive(Clone)]
pub struct SchedulerHandle {
    tx: std_mpsc::Sender<Request>,
}

impl SchedulerHandle {
    pub fn submit(&self, req: Request) -> Result<()> {
        self.tx
            .send(req)
            .map_err(|_| Error::Unavailable("engine loop has shut down".into()))
    }
}

/// Spawn the engine thread and return its handle.
pub fn start(model: Box<dyn BatchModel>, cfg: SchedulerConfig) -> SchedulerHandle {
    let (tx, rx) = std_mpsc::channel::<Request>();
    std::thread::Builder::new()
        .name("cgn-infer-engine".into())
        .spawn(move || {
            let mut sched = Scheduler::new(model, cfg);
            sched.run(rx);
        })
        .expect("spawn engine thread");
    SchedulerHandle { tx }
}

pub(crate) struct Scheduler {
    model: Box<dyn BatchModel>,
    cfg: SchedulerConfig,
    pool: KvPool,
    waiting: VecDeque<Sequence>,
    running: Vec<Sequence>,
    next_id: u64,
    tick: u64,
}

impl Scheduler {
    pub(crate) fn new(model: Box<dyn BatchModel>, mut cfg: SchedulerConfig) -> Self {
        cfg.max_batch = cfg.max_batch.clamp(1, model.max_batch());
        cfg.prefill_chunk = cfg.prefill_chunk.max(1);
        let pool = KvPool::new(KvPool::blocks_for(cfg.kv_pool_tokens).max(1));
        info!(
            max_batch = cfg.max_batch,
            prefill_chunk = cfg.prefill_chunk,
            kv_blocks = pool.total_blocks(),
            "scheduler started"
        );
        Self {
            model,
            cfg,
            pool,
            waiting: VecDeque::new(),
            running: Vec::new(),
            next_id: 1,
            tick: 0,
        }
    }

    fn run(&mut self, rx: std_mpsc::Receiver<Request>) {
        loop {
            // Block when idle; otherwise just drain what's pending.
            if self.waiting.is_empty() && self.running.is_empty() {
                match rx.recv() {
                    Ok(req) => self.enqueue(req),
                    Err(_) => return, // engine dropped
                }
            }
            while let Ok(req) = rx.try_recv() {
                self.enqueue(req);
            }
            self.step();
        }
    }

    fn enqueue(&mut self, req: Request) {
        let id = self.next_id;
        self.next_id += 1;
        let max_seq = self.model.max_seq_len();
        if req.prompt_tokens.is_empty() || req.prompt_tokens.len() >= max_seq {
            let _ = req.tx.blocking_send(StreamEvent {
                delta: String::new(),
                finish_reason: Some("error".into()),
                prompt_tokens: req.prompt_tokens.len(),
                completion_tokens: 0,
            });
            return;
        }
        let sampler = Sampler::new(req.params.clone());
        let prompt_len = req.prompt_tokens.len();
        let tokens = req.prompt_tokens.clone();
        self.waiting.push_back(Sequence {
            id,
            req,
            phase: Phase::Prefill { done: 0 },
            tokens,
            prompt_len,
            generated: Vec::new(),
            sampler,
            emitted_len: 0,
            admitted_at: 0,
        });
    }

    /// One scheduler iteration: admit, prefill one chunk, decode.
    /// Exposed to tests.
    pub(crate) fn step(&mut self) {
        self.admit();
        self.prefill_one_chunk();
        self.decode_step();
    }

    fn admit(&mut self) {
        while self.running.len() < self.cfg.max_batch {
            let Some(seq) = self.waiting.front() else {
                break;
            };
            // Reserve the whole prompt plus one decode block up front
            // so admission implies the prefill can complete.
            if !self.pool.reserve(seq.id, seq.prompt_len + 1) {
                break;
            }
            let mut seq = self.waiting.pop_front().expect("front checked");
            self.tick += 1;
            seq.admitted_at = self.tick;
            debug!(seq = seq.id, prompt = seq.prompt_len, "admitted");
            self.running.push(seq);
        }
    }

    /// Process one prefill chunk for the oldest prefilling sequence.
    fn prefill_one_chunk(&mut self) {
        let Some(idx) = self
            .running
            .iter()
            .position(|s| matches!(s.phase, Phase::Prefill { .. }))
        else {
            return;
        };
        let seq = &mut self.running[idx];
        let Phase::Prefill { done } = seq.phase else {
            unreachable!()
        };
        let remaining = seq.prompt_len - done;
        let take = remaining.min(self.cfg.prefill_chunk);
        let last_chunk = take == remaining;
        let chunk = seq.tokens[done..done + take].to_vec();
        let id = seq.id;
        match self.model.prefill(id, &chunk, done, last_chunk) {
            Ok(Some(mut logits)) => {
                // Prompt fully processed: sample the first token.
                let seq = &mut self.running[idx];
                seq.phase = Phase::Decode;
                let next = seq.sampler.sample(&mut logits, &seq.tokens);
                self.push_token(idx, next);
            }
            Ok(None) => {
                let seq = &mut self.running[idx];
                seq.phase = Phase::Prefill { done: done + take };
            }
            Err(e) => {
                warn!(seq = id, error = %e, "prefill failed");
                self.finish(idx, "error");
            }
        }
    }

    /// Advance all decode-phase sequences by one token.
    fn decode_step(&mut self) {
        // Each sequence grows by one token this step; make sure every
        // participant has KV room, preempting the youngest sequences
        // if the pool is full. Work with ids (not indices): preemption
        // reshuffles `running`.
        let candidates: Vec<(u64, usize)> = self
            .running
            .iter()
            .filter(|s| matches!(s.phase, Phase::Decode))
            .map(|s| (s.id, s.tokens.len() + 1))
            .collect();
        if candidates.is_empty() {
            return;
        }
        for (id, need) in candidates {
            if self.running.iter().any(|s| s.id == id) {
                let _ = self.ensure_room(id, need);
            }
        }
        // Build the final batch from sequences still running in
        // decode phase after any preemption.
        let batch: Vec<(usize, u64, u32)> = self
            .running
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s.phase, Phase::Decode))
            .map(|(i, s)| (i, s.id, *s.tokens.last().expect("non-empty")))
            .collect();
        if batch.is_empty() {
            return;
        }
        let model_batch: Vec<(u64, u32)> = batch.iter().map(|&(_, id, t)| (id, t)).collect();
        let all_logits = match self.model.decode(&model_batch) {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, "decode step failed");
                // Fail every participant; indices shift as we remove,
                // so walk from the back.
                for &(idx, _, _) in batch.iter().rev() {
                    self.finish(idx, "error");
                }
                return;
            }
        };
        // Sample + emit. Walk from the back so `finish` removals
        // don't shift pending indices.
        for (&(idx, _, _), mut logits) in batch.iter().zip(all_logits).rev() {
            let seq = &mut self.running[idx];
            let next = seq.sampler.sample(&mut logits, &seq.tokens);
            self.push_token(idx, next);
        }
    }

    /// Make sure `id` can hold `need` tokens of KV, preempting the
    /// most recently admitted *other* sequences if necessary. Returns
    /// false if room could not be made (the sequence itself is then
    /// preempted).
    fn ensure_room(&mut self, id: u64, need: usize) -> bool {
        loop {
            if self.pool.reserve(id, need) {
                return true;
            }
            // Preempt the youngest running sequence that isn't `id`.
            let victim = self
                .running
                .iter()
                .filter(|s| s.id != id)
                .max_by_key(|s| s.admitted_at)
                .map(|s| s.id);
            match victim {
                Some(v) => self.preempt(v),
                None => {
                    // Nothing left to evict: preempt `id` itself and
                    // hope for room later (it goes back to waiting).
                    warn!(seq = id, "kv pool exhausted; preempting requester");
                    self.preempt(id);
                    return false;
                }
            }
        }
    }

    /// Move a running sequence back to the wait queue, dropping its
    /// KV. Generated tokens are preserved: the re-queued "prompt" is
    /// prompt + generated-so-far.
    fn preempt(&mut self, id: u64) {
        let Some(idx) = self.running.iter().position(|s| s.id == id) else {
            return;
        };
        let mut seq = self.running.remove(idx);
        self.pool.release(seq.id);
        self.model.drop_seq(seq.id);
        debug!(seq = seq.id, generated = seq.generated.len(), "preempted");
        seq.phase = Phase::Prefill { done: 0 };
        // Re-prefill everything produced so far. Decode resumes from
        // the last generated token's logits.
        self.waiting.push_front(seq);
    }

    /// Append a sampled token to `running[idx]`, emit incremental
    /// text, and finish the sequence on EOS / stop string / budget.
    fn push_token(&mut self, idx: usize, token: u32) {
        let max_seq_len = self.model.max_seq_len();
        let seq = &mut self.running[idx];
        if Some(token) == seq.req.eos_token_id {
            self.finish(idx, "stop");
            return;
        }
        seq.generated.push(token);
        seq.tokens.push(token);

        let text = match seq.req.tokenizer.decode(&seq.generated, true) {
            Ok(t) => t,
            Err(e) => {
                warn!(seq = seq.id, error = %e, "detokenize failed");
                self.finish(idx, "error");
                return;
            }
        };

        // Stop-string hit anywhere in the decoded text?
        if let Some(hit) = seq
            .req
            .stop
            .iter()
            .filter_map(|s| text.find(s.as_str()))
            .min()
        {
            let delta = text[..hit].get(seq.emitted_len..).unwrap_or("").to_string();
            if !delta.is_empty() {
                seq.emitted_len = hit;
                let ev = StreamEvent {
                    delta,
                    finish_reason: None,
                    prompt_tokens: seq.prompt_len,
                    completion_tokens: seq.generated.len(),
                };
                let _ = seq.req.tx.blocking_send(ev);
            }
            self.finish(idx, "stop");
            return;
        }

        // Emit new complete text, holding back partial UTF-8 and
        // partial stop-string matches at the tail.
        if !text.ends_with('\u{FFFD}') && text.len() > seq.emitted_len {
            let safe_len = held_back_len(&text, &seq.req.stop);
            if safe_len > seq.emitted_len {
                let delta = text[seq.emitted_len..safe_len].to_string();
                seq.emitted_len = safe_len;
                let ev = StreamEvent {
                    delta,
                    finish_reason: None,
                    prompt_tokens: seq.prompt_len,
                    completion_tokens: seq.generated.len(),
                };
                if seq.req.tx.blocking_send(ev).is_err() {
                    // Client went away: stop generating for it.
                    self.finish_silent(idx);
                    return;
                }
            }
        }

        let budget = seq.budget(max_seq_len);
        if seq.generated.len() >= budget {
            self.finish(idx, "length");
        }
    }

    /// Remove `running[idx]`, flush held-back text, send the final
    /// frame, release resources.
    fn finish(&mut self, idx: usize, reason: &str) {
        let seq = self.running.remove(idx);
        self.pool.release(seq.id);
        self.model.drop_seq(seq.id);

        // Flush text held back for a stop-string match that never
        // completed (not on stop: the delta up to the match was
        // already flushed).
        if reason == "length" {
            if let Ok(text) = seq.req.tokenizer.decode(&seq.generated, true) {
                if text.len() > seq.emitted_len {
                    let _ = seq.req.tx.blocking_send(StreamEvent {
                        delta: text[seq.emitted_len..].to_string(),
                        finish_reason: None,
                        prompt_tokens: seq.prompt_len,
                        completion_tokens: seq.generated.len(),
                    });
                }
            }
        }
        let _ = seq.req.tx.blocking_send(StreamEvent {
            delta: String::new(),
            finish_reason: Some(reason.to_string()),
            prompt_tokens: seq.prompt_len,
            completion_tokens: seq.generated.len(),
        });
        debug!(
            seq = seq.id,
            reason,
            tokens = seq.generated.len(),
            "finished"
        );
    }

    /// Drop a sequence without emitting (client disconnected).
    fn finish_silent(&mut self, idx: usize) {
        let seq = self.running.remove(idx);
        self.pool.release(seq.id);
        self.model.drop_seq(seq.id);
    }

    #[cfg(test)]
    fn counts(&self) -> (usize, usize) {
        (self.waiting.len(), self.running.len())
    }
}

/// Length of the prefix of `text` that is safe to emit, holding back
/// any suffix that could still grow into one of `stops`.
pub fn held_back_len(text: &str, stops: &[String]) -> usize {
    let mut safe = text.len();
    for stop in stops {
        for start in text.char_indices().map(|(i, _)| i) {
            let tail = &text[start..];
            if stop.starts_with(tail) && start < safe {
                safe = start;
            }
        }
    }
    safe
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// Mock runtime: "logits" always favor token `next_token`, so
    /// generation is deterministic; records call patterns.
    struct MockModel {
        max_batch: usize,
        max_seq_len: usize,
        next_token: u32,
        vocab: usize,
        kv: HashMap<u64, usize>,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl MockModel {
        fn new(max_batch: usize, max_seq_len: usize) -> Self {
            Self {
                max_batch,
                max_seq_len,
                next_token: 5,
                vocab: 32,
                kv: HashMap::new(),
                log: Arc::new(Mutex::new(vec![])),
            }
        }

        fn logits(&self) -> Vec<f32> {
            let mut l = vec![0.0; self.vocab];
            l[self.next_token as usize] = 10.0;
            l
        }
    }

    impl BatchModel for MockModel {
        fn max_batch(&self) -> usize {
            self.max_batch
        }
        fn max_seq_len(&self) -> usize {
            self.max_seq_len
        }
        fn prefill(
            &mut self,
            seq: u64,
            tokens: &[u32],
            start_pos: usize,
            want_logits: bool,
        ) -> Result<Option<Vec<f32>>> {
            if start_pos == 0 {
                self.kv.insert(seq, 0);
            }
            let have = *self.kv.get(&seq).unwrap_or(&0);
            assert_eq!(have, start_pos, "prefill position contract violated");
            self.kv.insert(seq, start_pos + tokens.len());
            self.log
                .lock()
                .unwrap()
                .push(format!("prefill {seq} {} @{start_pos}", tokens.len()));
            Ok(want_logits.then(|| self.logits()))
        }
        fn decode(&mut self, batch: &[(u64, u32)]) -> Result<Vec<Vec<f32>>> {
            assert!(batch.len() <= self.max_batch);
            let ids: Vec<u64> = batch.iter().map(|b| b.0).collect();
            self.log.lock().unwrap().push(format!("decode {ids:?}"));
            for &(seq, _) in batch {
                *self.kv.get_mut(&seq).expect("decode of unknown seq") += 1;
            }
            Ok(batch.iter().map(|_| self.logits()).collect())
        }
        fn drop_seq(&mut self, seq: u64) {
            self.kv.remove(&seq);
        }
    }

    /// Identity-ish tokenizer for tests: builds a real tokenizers
    /// object from a tiny vocab where token id i decodes to "t<i> ".
    fn test_tokenizer() -> tokenizers::Tokenizer {
        use tokenizers::models::wordlevel::WordLevel;
        let vocab: std::collections::HashMap<String, u32> =
            (0..32u32).map(|i| (format!("t{i}"), i)).collect();
        let model = WordLevel::builder()
            .vocab(vocab)
            .unk_token("t0".into())
            .build()
            .unwrap();
        let mut tok = tokenizers::Tokenizer::new(model);
        tok.with_decoder(Some(tokenizers::decoders::wordpiece::WordPiece::new(
            "##".into(),
            false,
        )));
        tok
    }

    fn mk_request(prompt: Vec<u32>, max_tokens: usize) -> (Request, mpsc::Receiver<StreamEvent>) {
        let (tx, rx) = mpsc::channel(256);
        (
            Request {
                prompt_tokens: prompt,
                max_tokens,
                stop: vec![],
                params: SamplingParams {
                    temperature: 0.0,
                    ..Default::default()
                },
                tokenizer: test_tokenizer(),
                eos_token_id: None,
                tx,
            },
            rx,
        )
    }

    fn drain(rx: &mut mpsc::Receiver<StreamEvent>) -> (String, Option<String>, usize) {
        let mut text = String::new();
        let mut finish = None;
        let mut completion = 0;
        while let Ok(ev) = rx.try_recv() {
            text.push_str(&ev.delta);
            completion = ev.completion_tokens;
            if ev.finish_reason.is_some() {
                finish = ev.finish_reason;
            }
        }
        (text, finish, completion)
    }

    fn sched(model: MockModel, cfg: SchedulerConfig) -> Scheduler {
        Scheduler::new(Box::new(model), cfg)
    }

    #[test]
    fn single_request_runs_to_length() {
        let model = MockModel::new(4, 128);
        let mut s = sched(model, SchedulerConfig::default());
        let (req, mut rx) = mk_request(vec![1, 2, 3], 4);
        s.enqueue(req);
        for _ in 0..10 {
            s.step();
        }
        let (text, finish, completion) = drain(&mut rx);
        assert_eq!(finish.as_deref(), Some("length"));
        assert_eq!(completion, 4);
        assert!(text.contains("t5"));
        assert_eq!(s.counts(), (0, 0));
    }

    #[test]
    fn decode_is_batched_across_sequences() {
        let model = MockModel::new(4, 128);
        let log = model.log.clone();
        let mut s = sched(model, SchedulerConfig::default());
        let (r1, mut rx1) = mk_request(vec![1, 2], 3);
        let (r2, mut rx2) = mk_request(vec![3, 4], 3);
        s.enqueue(r1);
        s.enqueue(r2);
        for _ in 0..12 {
            s.step();
        }
        assert_eq!(drain(&mut rx1).1.as_deref(), Some("length"));
        assert_eq!(drain(&mut rx2).1.as_deref(), Some("length"));
        // At least one decode step must have carried both sequences.
        let log = log.lock().unwrap();
        assert!(
            log.iter().any(|l| l == "decode [1, 2]"),
            "no batched decode found in {log:?}"
        );
    }

    #[test]
    fn sequential_runtime_serializes() {
        let model = MockModel::new(1, 128); // max_batch = 1
        let log = model.log.clone();
        let mut s = sched(model, SchedulerConfig::default());
        assert_eq!(s.cfg.max_batch, 1);
        let (r1, mut rx1) = mk_request(vec![1], 2);
        let (r2, mut rx2) = mk_request(vec![2], 2);
        s.enqueue(r1);
        s.enqueue(r2);
        for _ in 0..12 {
            s.step();
        }
        assert_eq!(drain(&mut rx1).1.as_deref(), Some("length"));
        assert_eq!(drain(&mut rx2).1.as_deref(), Some("length"));
        // Never more than one sequence per decode call.
        for l in log.lock().unwrap().iter() {
            assert!(!l.contains(','), "batched call on sequential runtime: {l}");
        }
    }

    #[test]
    fn long_prompt_is_chunked() {
        let model = MockModel::new(4, 4096);
        let log = model.log.clone();
        let mut s = sched(
            model,
            SchedulerConfig {
                prefill_chunk: 8,
                ..Default::default()
            },
        );
        let (req, mut rx) = mk_request((0..20).collect(), 1);
        s.enqueue(req);
        for _ in 0..8 {
            s.step();
        }
        assert_eq!(drain(&mut rx).1.as_deref(), Some("length"));
        let log = log.lock().unwrap();
        let chunks: Vec<&String> = log.iter().filter(|l| l.starts_with("prefill")).collect();
        assert_eq!(
            chunks,
            vec!["prefill 1 8 @0", "prefill 1 8 @8", "prefill 1 4 @16"]
        );
    }

    #[test]
    fn admission_respects_kv_budget() {
        let model = MockModel::new(4, 4096);
        // Pool of 2 blocks = 32 tokens; each request needs 2 blocks
        // (17-token prompt + 1 growth token spills into block 2).
        let mut s = sched(
            model,
            SchedulerConfig {
                kv_pool_tokens: 32,
                ..Default::default()
            },
        );
        let (r1, _rx1) = mk_request((0..17).collect(), 100);
        let (r2, _rx2) = mk_request((0..17).collect(), 100);
        s.enqueue(r1);
        s.enqueue(r2);
        s.admit();
        // Only the first fits.
        assert_eq!(s.counts(), (1, 1));
    }

    #[test]
    fn pool_pressure_preempts_youngest_and_requeues() {
        let model = MockModel::new(4, 4096);
        // 3 blocks = 48 tokens. Two 17-token prompts (2 blocks each)
        // cannot both be resident once they grow.
        let mut s = sched(
            model,
            SchedulerConfig {
                kv_pool_tokens: 48,
                ..Default::default()
            },
        );
        let (r1, mut rx1) = mk_request((0..17).collect(), 100);
        let (r2, mut rx2) = mk_request((0..15).collect(), 100);
        s.enqueue(r1);
        s.enqueue(r2);
        // Run enough steps that seq 1 (17+n tokens) and seq 2 (15+n)
        // grow past the pool: someone must get preempted and later
        // both must still finish (they hit ctx? budget 100 each,
        // total 48-token pool → they alternate via preemption).
        for _ in 0..600 {
            s.step();
        }
        let (_, f1, _) = drain(&mut rx1);
        let (_, f2, _) = drain(&mut rx2);
        assert_eq!(f1.as_deref(), Some("length"));
        assert_eq!(f2.as_deref(), Some("length"));
    }

    #[test]
    fn eos_finishes_with_stop() {
        let model = MockModel::new(4, 128); // always emits token 5
        let mut s = sched(model, SchedulerConfig::default());
        let (mut req, mut rx) = mk_request(vec![1, 2], 10);
        req.eos_token_id = Some(5);
        s.enqueue(req);
        for _ in 0..5 {
            s.step();
        }
        let (text, finish, completion) = drain(&mut rx);
        assert_eq!(finish.as_deref(), Some("stop"));
        assert_eq!(completion, 0);
        assert!(text.is_empty());
    }

    #[test]
    fn stop_string_truncates_output() {
        let model = MockModel::new(4, 128); // emits "t5 t5 t5 ..."? decodes as "t5 t5"...
        let mut s = sched(model, SchedulerConfig::default());
        let (mut req, mut rx) = mk_request(vec![1, 2], 10);
        req.stop = vec!["t5 t5".into()];
        s.enqueue(req);
        for _ in 0..20 {
            s.step();
        }
        let (text, finish, _) = drain(&mut rx);
        assert_eq!(finish.as_deref(), Some("stop"));
        assert!(!text.contains("t5 t5"), "stop string leaked into: {text:?}");
    }

    #[test]
    fn oversized_prompt_errors_immediately() {
        let model = MockModel::new(4, 16);
        let mut s = sched(model, SchedulerConfig::default());
        let (req, mut rx) = mk_request((0..16).collect(), 4);
        s.enqueue(req);
        let (_, finish, _) = drain(&mut rx);
        assert_eq!(finish.as_deref(), Some("error"));
        assert_eq!(s.counts(), (0, 0));
    }

    #[test]
    fn holds_back_partial_stop_match() {
        let stops = vec!["</s>".to_string()];
        assert_eq!(held_back_len("hello </", &stops), 6);
        assert_eq!(held_back_len("hello <", &stops), 6);
        assert_eq!(held_back_len("hello", &stops), 5);
        assert_eq!(held_back_len("hello", &[]), 5);
    }
}
