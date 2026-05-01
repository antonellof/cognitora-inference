//! Render the engine command-line from an [`EngineConfig`] + [`ModelSpec`].
//!
//! Each engine kind has its own argv shape:
//!
//! * **vllm**       `vllm serve <model> --tensor-parallel-size <tp>
//!                  [--max-model-len <len>] [extra ...]`
//! * **llama_cpp**  python_server: `python -m llama_cpp.server --host <h>
//!                  --port <p> --model <gguf> --model_alias <name>
//!                  --n_ctx <ctx> --n_threads <n> [--n_gpu_layers <k>]
//!                  [extra ...]`
//!                  binary mode: `<binary> --model <gguf> --host <h>
//!                  --port <p> [extra ...]`
//! * **openai_compat** — no spawn; caller checks `should_spawn()`.

use cgn_core::{
    config::{EngineConfig, EngineKind, LlamaCppMode},
    Error, Result,
};

use super::ModelSpec;

/// True iff the supervisor should fork an engine process.
pub fn should_spawn(cfg: &EngineConfig) -> bool {
    !matches!(cfg.kind, EngineKind::OpenaiCompat)
}

/// Render the argv for the engine child process.
///
/// `legacy_cmd` is the deprecated `[agent].vllm_cmd` array. When set, it
/// wins over the auto-rendered vLLM argv (kept for back-compat with the
/// pre-`[engine]` config schema).
pub fn render_argv(
    cfg: &EngineConfig,
    spec: &ModelSpec,
    legacy_cmd: Option<&[String]>,
) -> Result<Vec<String>> {
    if let Some(legacy) = legacy_cmd {
        if !legacy.is_empty() {
            return Ok(render_legacy(legacy, spec));
        }
    }
    match cfg.kind {
        EngineKind::Vllm => Ok(render_vllm(cfg, spec)),
        EngineKind::LlamaCpp => render_llama_cpp(cfg, spec),
        EngineKind::OpenaiCompat => Err(Error::Config(
            "engine.kind = openai_compat does not spawn — caller should check should_spawn()"
                .into(),
        )),
    }
}

fn render_vllm(cfg: &EngineConfig, spec: &ModelSpec) -> Vec<String> {
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
    argv.extend(cfg.vllm.extra_args.clone());
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

#[cfg(test)]
mod tests {
    use super::*;
    use cgn_core::config::{LlamaCppEngineConfig, VllmEngineConfig};
    use std::path::PathBuf;

    fn vllm_cfg() -> EngineConfig {
        EngineConfig {
            kind: EngineKind::Vllm,
            url: "http://127.0.0.1:8000".into(),
            vllm: VllmEngineConfig {
                binary: "vllm".into(),
                extra_args: vec!["--enable-chunked-prefill".into()],
            },
            llama_cpp: LlamaCppEngineConfig::default(),
        }
    }

    fn llama_cfg() -> EngineConfig {
        EngineConfig {
            kind: EngineKind::LlamaCpp,
            url: "http://127.0.0.1:8000".into(),
            vllm: VllmEngineConfig::default(),
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
        let argv = render_argv(&vllm_cfg(), &spec("Qwen/Qwen2.5-0.5B", None), None).unwrap();
        assert_eq!(argv[0], "vllm");
        assert_eq!(argv[1], "serve");
        assert_eq!(argv[2], "Qwen/Qwen2.5-0.5B");
        assert!(argv.contains(&"--tensor-parallel-size".to_string()));
        assert!(argv.contains(&"--max-model-len".to_string()));
        assert!(argv.contains(&"--enable-chunked-prefill".to_string()));
    }

    #[test]
    fn renders_llama_cpp_python_server() {
        let argv = render_argv(
            &llama_cfg(),
            &spec("Qwen/Qwen2.5-0.5B", Some("/tmp/qwen.gguf")),
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
        let err = render_argv(&llama_cfg(), &spec("Qwen/Qwen2.5-0.5B", None), None).unwrap_err();
        assert!(format!("{err:?}").contains("path"));
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
            vllm: VllmEngineConfig::default(),
            llama_cpp: LlamaCppEngineConfig::default(),
        };
        assert!(!should_spawn(&cfg));
        assert!(render_argv(&cfg, &spec("a", None), None).is_err());
    }
}
