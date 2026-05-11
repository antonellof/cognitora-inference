//! Render the engine command-line from an [`EngineConfig`] + [`ModelSpec`].
//!
//! Each engine kind has its own argv shape:
//!
//! * **vllm** — `vllm serve <model> --tensor-parallel-size <tp>
//!   [--max-model-len <len>] [--kv-transfer-config <json>] [extra ...]`
//! * **sglang** — `python -m sglang.launch_server --model-path <model>
//!   --tp <tp> --host <h> --port <p> --context-length <ctx>
//!   --mem-fraction-static <frac>
//!   [--enable-hierarchical-cache --hicache-* ...] [extra ...]`
//! * **llama_cpp** — `python_server` mode: `python -m llama_cpp.server
//!   --host <h> --port <p> --model <gguf> --model_alias <name>
//!   --n_ctx <ctx> --n_threads <n> [--n_gpu_layers <k>] [extra ...]`.
//!   `binary` mode: `<binary> --model <gguf> --host <h> --port <p>
//!   [extra ...]`.
//! * **mlx** — `python -m mlx_lm.server --model <hf_or_path> --host <h>
//!   --port <p> [extra ...]` (Apple Silicon only).
//! * **openai_compat** — no spawn; caller checks `should_spawn()`.
//!
//! KV offload mapping (driven by `engine.kv_offload` + `agent.role`):
//!
//! | engine  | role     | offload     | injected flags                                         |
//! |---------|----------|-------------|--------------------------------------------------------|
//! | vllm    | both     | none        | (nothing)                                              |
//! | vllm    | both     | nixl        | `--kv-transfer-config '{NixlConnector,kv_both}'`       |
//! | vllm    | prefill  | nixl        | `--kv-transfer-config '{NixlConnector,kv_producer}'`   |
//! | vllm    | decode   | nixl        | `--kv-transfer-config '{NixlConnector,kv_consumer}'`   |
//! | vllm    | both     | lmcache     | `--kv-transfer-config '{LMCacheConnectorV1,kv_both}'`  |
//! | vllm    | prefill  | lmcache     | `--kv-transfer-config '{PdConnector(LMCache+Nixl)}'`   |
//! | vllm    | decode   | lmcache     | `--kv-transfer-config '{NixlConnector,kv_consumer}'`   |
//! | vllm    | both     | kvbm        | `--kv-transfer-config '{DynamoConnector(kvbm),kv_both}'`|
//! | sglang  | both     | hicache     | `--enable-hierarchical-cache --hicache-* ...`          |
//! | llama_cpp / mlx / openai_compat | * | only `none` is valid                          |
//!
//! Combinations not in the table are rejected at render time.

use cgn_core::{
    config::{EngineConfig, EngineKind, KvOffload, LlamaCppMode, NodeRoleCfg},
    Error, Result,
};

use super::ModelSpec;

/// True iff the supervisor should fork an engine process.
pub fn should_spawn(cfg: &EngineConfig) -> bool {
    !matches!(cfg.kind, EngineKind::OpenaiCompat)
}

/// Render the argv for the engine child process.
///
/// `role` is the agent's disagg role; it influences the
/// `--kv-transfer-config` JSON for vLLM (prefill = producer, decode =
/// consumer, both = symmetric).
///
/// `legacy_cmd` is the deprecated `[agent].vllm_cmd` array. When set, it
/// wins over the auto-rendered vLLM argv (kept for back-compat with the
/// pre-`[engine]` config schema).
pub fn render_argv(
    cfg: &EngineConfig,
    spec: &ModelSpec,
    role: NodeRoleCfg,
    legacy_cmd: Option<&[String]>,
) -> Result<Vec<String>> {
    validate_kv_offload(cfg.kind, cfg.kv_offload)?;
    if let Some(legacy) = legacy_cmd {
        if !legacy.is_empty() {
            return Ok(render_legacy(legacy, spec));
        }
    }
    match cfg.kind {
        EngineKind::Vllm => Ok(render_vllm(cfg, spec, role)),
        EngineKind::Sglang => Ok(render_sglang(cfg, spec)),
        EngineKind::LlamaCpp => render_llama_cpp(cfg, spec),
        EngineKind::Mlx => Ok(render_mlx(cfg, spec)),
        EngineKind::OpenaiCompat => Err(Error::Config(
            "engine.kind = openai_compat does not spawn — caller should check should_spawn()"
                .into(),
        )),
    }
}

fn render_vllm(cfg: &EngineConfig, spec: &ModelSpec, role: NodeRoleCfg) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        cfg.vllm.binary.clone(),
        "serve".into(),
        spec.name.clone(),
        "--tensor-parallel-size".into(),
        spec.tp.to_string(),
    ];
    if let Some(len) = spec.max_model_len {
        argv.push("--max-model-len".into());
        argv.push(len.to_string());
    }
    if let Some(json) = vllm_kv_transfer_config(role, cfg.kv_offload) {
        argv.push("--kv-transfer-config".into());
        argv.push(json);
    }
    argv.extend(cfg.vllm.extra_args.clone());
    argv.extend(spec.extra_args.clone());
    argv
}

fn render_sglang(cfg: &EngineConfig, spec: &ModelSpec) -> Vec<String> {
    let model_path = spec
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| spec.name.clone());

    let ctx_len = spec.max_model_len.unwrap_or(cfg.sglang.context_length);
    let mut argv: Vec<String> = vec![
        cfg.sglang.binary.clone(),
        "-m".into(),
        "sglang.launch_server".into(),
        "--model-path".into(),
        model_path,
        "--served-model-name".into(),
        spec.name.clone(),
        "--tp".into(),
        spec.tp.to_string(),
        "--host".into(),
        cfg.sglang.host.clone(),
        "--port".into(),
        cfg.sglang.port.to_string(),
        "--context-length".into(),
        ctx_len.to_string(),
        "--mem-fraction-static".into(),
        format!("{:.3}", cfg.sglang.mem_fraction_static),
    ];
    argv.extend(sglang_hicache_args(cfg.kv_offload));
    argv.extend(cfg.sglang.extra_args.clone());
    argv.extend(spec.extra_args.clone());
    argv
}

fn render_llama_cpp(cfg: &EngineConfig, spec: &ModelSpec) -> Result<Vec<String>> {
    let path = spec
        .path
        .as_ref()
        .ok_or_else(|| {
            Error::Config(format!(
                "engine.kind = llama_cpp requires [models.\"{}\"].path = <gguf>",
                spec.name
            ))
        })?
        .display()
        .to_string();

    let mut argv: Vec<String> = match cfg.llama_cpp.mode {
        LlamaCppMode::PythonServer => vec![
            cfg.llama_cpp.binary.clone(),
            "-m".into(),
            "llama_cpp.server".into(),
            "--host".into(),
            cfg.llama_cpp.host.clone(),
            "--port".into(),
            cfg.llama_cpp.port.to_string(),
            "--model".into(),
            path,
            "--model_alias".into(),
            spec.name.clone(),
            "--n_ctx".into(),
            spec.max_model_len
                .map(|n| n.to_string())
                .unwrap_or_else(|| cfg.llama_cpp.n_ctx.to_string()),
            "--n_threads".into(),
            cfg.llama_cpp.n_threads.to_string(),
            "--n_gpu_layers".into(),
            cfg.llama_cpp.n_gpu_layers.to_string(),
        ],
        LlamaCppMode::Binary => vec![
            cfg.llama_cpp.binary.clone(),
            "--model".into(),
            path,
            "--host".into(),
            cfg.llama_cpp.host.clone(),
            "--port".into(),
            cfg.llama_cpp.port.to_string(),
            "--ctx-size".into(),
            spec.max_model_len
                .map(|n| n.to_string())
                .unwrap_or_else(|| cfg.llama_cpp.n_ctx.to_string()),
            "--threads".into(),
            cfg.llama_cpp.n_threads.to_string(),
            "--n-gpu-layers".into(),
            cfg.llama_cpp.n_gpu_layers.to_string(),
        ],
    };
    argv.extend(cfg.llama_cpp.extra_args.clone());
    argv.extend(spec.extra_args.clone());
    Ok(argv)
}

/// `mlx_lm.server` — OpenAI-compatible HTTP on `host:port` (see mlx-lm `SERVER.md`).
fn render_mlx(cfg: &EngineConfig, spec: &ModelSpec) -> Vec<String> {
    let model = spec
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| spec.name.clone());

    let mut argv: Vec<String> = vec![
        cfg.mlx_lm.binary.clone(),
        "-m".into(),
        "mlx_lm.server".into(),
        "--model".into(),
        model,
        "--host".into(),
        cfg.mlx_lm.host.clone(),
        "--port".into(),
        cfg.mlx_lm.port.to_string(),
    ];
    argv.extend(cfg.mlx_lm.extra_args.clone());
    argv.extend(spec.extra_args.clone());
    argv
}

fn render_legacy(template: &[String], spec: &ModelSpec) -> Vec<String> {
    let mut argv: Vec<String> = template
        .iter()
        .map(|a| {
            a.replace("{model}", &spec.name)
                .replace("{tp}", &spec.tp.to_string())
        })
        .collect();
    if let Some(len) = spec.max_model_len {
        argv.push("--max-model-len".into());
        argv.push(len.to_string());
    }
    argv.extend(spec.extra_args.clone());
    argv
}

// ---------------------------------------------------------------------------
// KV offload renderers
// ---------------------------------------------------------------------------

/// Reject engine × kv_offload combinations that don't make sense.
///
/// The error is shaped so it surfaces clearly in `cgn-agent` start-up
/// logs and in `cgn-ctl recipe up` output.
fn validate_kv_offload(kind: EngineKind, offload: KvOffload) -> Result<()> {
    let ok = matches!(
        (kind, offload),
        (_, KvOffload::None)
            | (EngineKind::Vllm, KvOffload::Nixl)
            | (EngineKind::Vllm, KvOffload::Lmcache)
            | (EngineKind::Vllm, KvOffload::Kvbm)
            | (EngineKind::Sglang, KvOffload::Hicache)
            | (EngineKind::Sglang, KvOffload::Nixl)
    );
    if !ok {
        return Err(Error::Config(format!(
            "engine.kv_offload = \"{}\" is not supported with engine.kind = \"{}\". \
             Valid pairings: vllm × {{none,nixl,lmcache,kvbm}}, sglang × {{none,nixl,hicache}}, \
             llama_cpp/mlx/openai_compat × {{none}}.",
            offload.as_str(),
            match kind {
                EngineKind::Vllm => "vllm",
                EngineKind::Sglang => "sglang",
                EngineKind::LlamaCpp => "llama_cpp",
                EngineKind::Mlx => "mlx",
                EngineKind::OpenaiCompat => "openai_compat",
            }
        )));
    }
    Ok(())
}

/// Build the JSON blob for vLLM's `--kv-transfer-config`.
///
/// Returns `None` when no connector should be injected (the agent's
/// engine config opted out, or the role/offload combo is a no-op like
/// `Both × Nixl` in pure aggregated mode).
pub(crate) fn vllm_kv_transfer_config(role: NodeRoleCfg, offload: KvOffload) -> Option<String> {
    use KvOffload::*;
    use NodeRoleCfg::*;
    let json = match (role, offload) {
        (_, None) => return Option::None,
        (_, Hicache) => return Option::None, // never valid for vllm; caught by validate
        (Both, Nixl) => r#"{"kv_connector":"NixlConnector","kv_role":"kv_both"}"#.to_string(),
        (Prefill, Nixl) => r#"{"kv_connector":"NixlConnector","kv_role":"kv_producer"}"#.to_string(),
        (Decode, Nixl) => r#"{"kv_connector":"NixlConnector","kv_role":"kv_consumer"}"#.to_string(),
        (Both, Lmcache) => {
            r#"{"kv_connector":"LMCacheConnectorV1","kv_role":"kv_both"}"#.to_string()
        }
        (Prefill, Lmcache) => {
            // Dynamo's pattern: prefill worker stacks LMCache (offload) +
            // Nixl (handoff to decode). See
            // .temp/dynamo/docs/integrations/lmcache-integration.md.
            r#"{"kv_connector":"PdConnector","kv_role":"kv_both","kv_connector_extra_config":{"connectors":[{"kv_connector":"LMCacheConnectorV1","kv_role":"kv_both"},{"kv_connector":"NixlConnector","kv_role":"kv_both"}]}}"#.to_string()
        }
        (Decode, Lmcache) => {
            // Decode worker only needs Nixl; LMCache lives on the prefill side.
            r#"{"kv_connector":"NixlConnector","kv_role":"kv_both"}"#.to_string()
        }
        (Both, Kvbm) => r#"{"kv_connector":"DynamoConnector","kv_role":"kv_both","kv_connector_module_path":"kvbm.vllm_integration.connector"}"#.to_string(),
        (Prefill, Kvbm) => r#"{"kv_connector":"DynamoConnector","kv_role":"kv_producer","kv_connector_module_path":"kvbm.vllm_integration.connector"}"#.to_string(),
        (Decode, Kvbm) => r#"{"kv_connector":"DynamoConnector","kv_role":"kv_consumer","kv_connector_module_path":"kvbm.vllm_integration.connector"}"#.to_string(),
    };
    Some(json)
}

/// Build SGLang HiCache flags.
///
/// We pick conservative defaults that match the dynamo + SGLang docs
/// (`hicache-ratio = 2`, write-through, NIXL backend). Callers that
/// want a different storage backend (e.g. Mooncake) override via
/// `[engine.sglang].extra_args`.
pub(crate) fn sglang_hicache_args(offload: KvOffload) -> Vec<String> {
    match offload {
        KvOffload::Hicache => vec![
            "--enable-hierarchical-cache".into(),
            "--hicache-ratio".into(),
            "2".into(),
            "--hicache-write-policy".into(),
            "write_through".into(),
            "--hicache-storage-backend".into(),
            "nixl".into(),
        ],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cgn_core::config::{
        LlamaCppEngineConfig, MlxLmEngineConfig, SglangEngineConfig, VllmEngineConfig,
    };
    use std::path::PathBuf;

    fn vllm_cfg() -> EngineConfig {
        EngineConfig {
            kind: EngineKind::Vllm,
            url: "http://127.0.0.1:8000".into(),
            kv_offload: KvOffload::None,
            vllm: VllmEngineConfig {
                binary: "vllm".into(),
                extra_args: vec!["--enable-chunked-prefill".into()],
            },
            sglang: SglangEngineConfig::default(),
            llama_cpp: LlamaCppEngineConfig::default(),
            mlx_lm: Default::default(),
        }
    }

    fn sglang_cfg() -> EngineConfig {
        EngineConfig {
            kind: EngineKind::Sglang,
            url: "http://127.0.0.1:30000".into(),
            kv_offload: KvOffload::None,
            vllm: VllmEngineConfig::default(),
            sglang: SglangEngineConfig {
                binary: "python".into(),
                host: "127.0.0.1".into(),
                port: 30000,
                context_length: 8192,
                mem_fraction_static: 0.85,
                extra_args: vec!["--enable-torch-compile".into()],
            },
            llama_cpp: LlamaCppEngineConfig::default(),
            mlx_lm: Default::default(),
        }
    }

    fn llama_cfg() -> EngineConfig {
        EngineConfig {
            kind: EngineKind::LlamaCpp,
            url: "http://127.0.0.1:8000".into(),
            kv_offload: KvOffload::None,
            vllm: VllmEngineConfig::default(),
            sglang: SglangEngineConfig::default(),
            llama_cpp: LlamaCppEngineConfig {
                binary: "python".into(),
                mode: LlamaCppMode::PythonServer,
                host: "127.0.0.1".into(),
                port: 8000,
                n_ctx: 4096,
                n_threads: 4,
                n_gpu_layers: 0,
                extra_args: vec![],
            },
            mlx_lm: Default::default(),
        }
    }

    fn mlx_cfg() -> EngineConfig {
        EngineConfig {
            kind: EngineKind::Mlx,
            url: "http://127.0.0.1:8090".into(),
            kv_offload: KvOffload::None,
            vllm: VllmEngineConfig::default(),
            sglang: SglangEngineConfig::default(),
            llama_cpp: LlamaCppEngineConfig::default(),
            mlx_lm: MlxLmEngineConfig::default(),
        }
    }

    fn spec(name: &str, path: Option<&str>) -> ModelSpec {
        ModelSpec {
            name: name.into(),
            tp: 1,
            max_model_len: Some(2048),
            extra_args: vec![],
            path: path.map(PathBuf::from),
        }
    }

    #[test]
    fn renders_vllm_command() {
        let argv = render_argv(
            &vllm_cfg(),
            &spec("Qwen/Qwen2.5-0.5B", None),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap();
        assert_eq!(argv[0], "vllm");
        assert_eq!(argv[1], "serve");
        assert_eq!(argv[2], "Qwen/Qwen2.5-0.5B");
        assert!(argv.contains(&"--tensor-parallel-size".to_string()));
        assert!(argv.contains(&"--max-model-len".to_string()));
        assert!(argv.contains(&"--enable-chunked-prefill".to_string()));
        assert!(!argv.iter().any(|a| a == "--kv-transfer-config"));
    }

    #[test]
    fn renders_sglang_command() {
        let argv = render_argv(
            &sglang_cfg(),
            &spec("Qwen/Qwen2.5-7B-Instruct", None),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap();
        assert_eq!(argv[0], "python");
        assert_eq!(argv[1], "-m");
        assert_eq!(argv[2], "sglang.launch_server");
        assert!(argv.contains(&"--model-path".to_string()));
        assert!(argv.contains(&"Qwen/Qwen2.5-7B-Instruct".to_string()));
        assert!(argv.contains(&"--served-model-name".to_string()));
        assert!(argv.contains(&"--tp".to_string()));
        assert!(argv.contains(&"--host".to_string()));
        assert!(argv.contains(&"--port".to_string()));
        assert!(argv.contains(&"--mem-fraction-static".to_string()));
        assert!(argv.contains(&"--enable-torch-compile".to_string()));
        assert!(!argv.iter().any(|a| a == "--enable-hierarchical-cache"));
    }

    #[test]
    fn sglang_uses_local_path_when_present() {
        let argv = render_argv(
            &sglang_cfg(),
            &spec("Qwen/Qwen2.5-7B-Instruct", Some("/models/qwen-7b")),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap();
        assert!(argv.contains(&"/models/qwen-7b".to_string()));
        assert!(argv.contains(&"Qwen/Qwen2.5-7B-Instruct".to_string()));
    }

    #[test]
    fn vllm_lmcache_aggregated_injects_lmcache_connector() {
        let mut cfg = vllm_cfg();
        cfg.kv_offload = KvOffload::Lmcache;
        let argv = render_argv(
            &cfg,
            &spec("Qwen/Qwen2.5-0.5B", None),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap();
        let i = argv
            .iter()
            .position(|a| a == "--kv-transfer-config")
            .expect("--kv-transfer-config missing");
        let json = &argv[i + 1];
        assert!(json.contains("LMCacheConnectorV1"));
        assert!(json.contains("kv_both"));
    }

    #[test]
    fn vllm_lmcache_prefill_uses_pd_connector_with_nixl() {
        let mut cfg = vllm_cfg();
        cfg.kv_offload = KvOffload::Lmcache;
        let argv = render_argv(
            &cfg,
            &spec("Qwen/Qwen2.5-0.5B", None),
            NodeRoleCfg::Prefill,
            None,
        )
        .unwrap();
        let i = argv
            .iter()
            .position(|a| a == "--kv-transfer-config")
            .unwrap();
        let json = &argv[i + 1];
        assert!(json.contains("PdConnector"));
        assert!(json.contains("LMCacheConnectorV1"));
        assert!(json.contains("NixlConnector"));
    }

    #[test]
    fn vllm_nixl_decode_emits_consumer_role() {
        let mut cfg = vllm_cfg();
        cfg.kv_offload = KvOffload::Nixl;
        let argv = render_argv(
            &cfg,
            &spec("Qwen/Qwen2.5-0.5B", None),
            NodeRoleCfg::Decode,
            None,
        )
        .unwrap();
        let i = argv
            .iter()
            .position(|a| a == "--kv-transfer-config")
            .unwrap();
        let json = &argv[i + 1];
        assert!(json.contains("NixlConnector"));
        assert!(json.contains("kv_consumer"));
    }

    #[test]
    fn vllm_kvbm_aggregated_uses_dynamo_connector() {
        let mut cfg = vllm_cfg();
        cfg.kv_offload = KvOffload::Kvbm;
        let argv = render_argv(
            &cfg,
            &spec("Qwen/Qwen2.5-0.5B", None),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap();
        let i = argv
            .iter()
            .position(|a| a == "--kv-transfer-config")
            .unwrap();
        let json = &argv[i + 1];
        assert!(json.contains("DynamoConnector"));
        assert!(json.contains("kvbm.vllm_integration.connector"));
    }

    #[test]
    fn sglang_hicache_appends_hierarchical_cache_flags() {
        let mut cfg = sglang_cfg();
        cfg.kv_offload = KvOffload::Hicache;
        let argv = render_argv(
            &cfg,
            &spec("Qwen/Qwen2.5-7B-Instruct", None),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap();
        assert!(argv.contains(&"--enable-hierarchical-cache".to_string()));
        assert!(argv.contains(&"--hicache-ratio".to_string()));
        assert!(argv.contains(&"--hicache-write-policy".to_string()));
        assert!(argv.contains(&"--hicache-storage-backend".to_string()));
        assert!(argv.contains(&"nixl".to_string()));
    }

    #[test]
    fn rejects_lmcache_on_sglang() {
        let mut cfg = sglang_cfg();
        cfg.kv_offload = KvOffload::Lmcache;
        let err = render_argv(
            &cfg,
            &spec("Qwen/Qwen2.5-7B-Instruct", None),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("kv_offload"));
        assert!(msg.contains("sglang"));
    }

    #[test]
    fn rejects_hicache_on_vllm() {
        let mut cfg = vllm_cfg();
        cfg.kv_offload = KvOffload::Hicache;
        let err = render_argv(
            &cfg,
            &spec("Qwen/Qwen2.5-0.5B", None),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("kv_offload"));
    }

    #[test]
    fn rejects_kvbm_on_llama_cpp() {
        let mut cfg = llama_cfg();
        cfg.kv_offload = KvOffload::Kvbm;
        let err = render_argv(
            &cfg,
            &spec("Qwen/Qwen2.5-0.5B", Some("/tmp/qwen.gguf")),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("kv_offload"));
    }

    #[test]
    fn renders_llama_cpp_python_server() {
        let argv = render_argv(
            &llama_cfg(),
            &spec("Qwen/Qwen2.5-0.5B", Some("/tmp/qwen.gguf")),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap();
        assert_eq!(argv[0], "python");
        assert_eq!(argv[1], "-m");
        assert_eq!(argv[2], "llama_cpp.server");
        assert!(argv.contains(&"--model".to_string()));
        assert!(argv.contains(&"/tmp/qwen.gguf".to_string()));
        assert!(argv.contains(&"--model_alias".to_string()));
        assert!(argv.contains(&"--n_gpu_layers".to_string()));
    }

    #[test]
    fn llama_cpp_requires_path() {
        let err = render_argv(
            &llama_cfg(),
            &spec("Qwen/Qwen2.5-0.5B", None),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("path"));
    }

    #[test]
    fn renders_mlx_command() {
        let argv = render_argv(
            &mlx_cfg(),
            &spec("mlx-community/Meta-Llama-3.2-3B-Instruct-4bit", None),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap();
        assert_eq!(argv[0], "python3");
        assert_eq!(argv[1], "-m");
        assert_eq!(argv[2], "mlx_lm.server");
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--model" && w[1] == "mlx-community/Meta-Llama-3.2-3B-Instruct-4bit"));
        assert!(argv.windows(2).any(|w| w[0] == "--port" && w[1] == "8090"));
    }

    #[test]
    fn mlx_uses_local_path_when_present() {
        let argv = render_argv(
            &mlx_cfg(),
            &spec(
                "mlx-community/Meta-Llama-3.2-3B-Instruct-4bit",
                Some("/models/my-mlx"),
            ),
            NodeRoleCfg::Both,
            None,
        )
        .unwrap();
        assert!(argv.contains(&"/models/my-mlx".to_string()));
    }

    #[test]
    fn legacy_cmd_takes_precedence() {
        let legacy = vec![
            "/bin/sleep".to_string(),
            "infinity".to_string(),
            "{model}".to_string(),
        ];
        let argv = render_argv(
            &vllm_cfg(),
            &spec("Qwen/Qwen2.5-0.5B", None),
            NodeRoleCfg::Both,
            Some(&legacy),
        )
        .unwrap();
        assert_eq!(argv[0], "/bin/sleep");
        assert_eq!(argv[2], "Qwen/Qwen2.5-0.5B");
    }

    #[test]
    fn openai_compat_does_not_spawn() {
        let cfg = EngineConfig {
            kind: EngineKind::OpenaiCompat,
            url: "http://127.0.0.1:8000".into(),
            kv_offload: KvOffload::None,
            vllm: VllmEngineConfig::default(),
            sglang: SglangEngineConfig::default(),
            llama_cpp: LlamaCppEngineConfig::default(),
            mlx_lm: MlxLmEngineConfig::default(),
        };
        assert!(!should_spawn(&cfg));
        assert!(render_argv(&cfg, &spec("a", None), NodeRoleCfg::Both, None).is_err());
    }
}
