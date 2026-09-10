//! Chat template rendering: turn OpenAI-style `messages` into the
//! model's prompt string.
//!
//! GGUF files embed the model's Jinja template
//! (`tokenizer.chat_template`); we render it with minijinja using the
//! same variables HF's `apply_chat_template` provides. When no
//! template is embedded we fall back to ChatML, which llama.cpp also
//! does and which every recent instruct model tolerates.

use cgn_core::{Error, Result};
use serde::Serialize;

/// One chat turn, matching the OpenAI request shape.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

const CHATML_FALLBACK: &str = "{% for message in messages %}<|im_start|>{{ message.role }}\n{{ message.content }}<|im_end|>\n{% endfor %}{% if add_generation_prompt %}<|im_start|>assistant\n{% endif %}";

pub struct ChatTemplate {
    env: minijinja::Environment<'static>,
}

impl ChatTemplate {
    /// Compile the embedded template, or the ChatML fallback if the
    /// GGUF has none.
    pub fn new(
        template: Option<String>,
        bos_token: Option<String>,
        eos_token: Option<String>,
    ) -> Result<Self> {
        let source = template.unwrap_or_else(|| CHATML_FALLBACK.to_string());
        let mut env = minijinja::Environment::new();
        env.add_global("bos_token", bos_token.unwrap_or_default());
        env.add_global("eos_token", eos_token.unwrap_or_default());
        // `raise_exception` appears in most HF templates for
        // unsupported role orders.
        env.add_function(
            "raise_exception",
            |msg: String| -> std::result::Result<String, minijinja::Error> {
                Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    msg,
                ))
            },
        );
        env.add_template_owned("chat".to_string(), source)
            .map_err(|e| Error::Config(format!("chat template: {e}")))?;
        Ok(Self { env })
    }

    /// Render messages into the prompt string, appending the
    /// generation prompt for the assistant turn.
    pub fn render(&self, messages: &[ChatMessage]) -> Result<String> {
        let tmpl = self
            .env
            .get_template("chat")
            .map_err(|e| Error::Internal(format!("chat template: {e}")))?;
        tmpl.render(minijinja::context! {
            messages => messages,
            add_generation_prompt => true,
        })
        .map_err(|e| Error::InvalidArgument(format!("chat template render: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs() -> Vec<ChatMessage> {
        vec![
            ChatMessage {
                role: "system".into(),
                content: "You are helpful.".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Hi!".into(),
            },
        ]
    }

    #[test]
    fn chatml_fallback_renders() {
        let t = ChatTemplate::new(None, None, None).unwrap();
        let out = t.render(&msgs()).unwrap();
        assert_eq!(
            out,
            "<|im_start|>system\nYou are helpful.<|im_end|>\n\
             <|im_start|>user\nHi!<|im_end|>\n\
             <|im_start|>assistant\n"
        );
    }

    #[test]
    fn llama3_style_template_renders() {
        let tmpl = "{{ bos_token }}{% for message in messages %}<|start_header_id|>{{ message['role'] }}<|end_header_id|>\n\n{{ message['content'] }}<|eot_id|>{% endfor %}{% if add_generation_prompt %}<|start_header_id|>assistant<|end_header_id|>\n\n{% endif %}";
        let t =
            ChatTemplate::new(Some(tmpl.into()), Some("<|begin_of_text|>".into()), None).unwrap();
        let out = t.render(&msgs()).unwrap();
        assert!(out.starts_with("<|begin_of_text|><|start_header_id|>system<|end_header_id|>"));
        assert!(out.contains("Hi!<|eot_id|>"));
        assert!(out.ends_with("<|start_header_id|>assistant<|end_header_id|>\n\n"));
    }

    #[test]
    fn raise_exception_maps_to_invalid_argument() {
        let tmpl = "{{ raise_exception('nope') }}";
        let t = ChatTemplate::new(Some(tmpl.into()), None, None).unwrap();
        let err = t.render(&msgs()).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "{err}");
    }

    #[test]
    fn bad_template_fails_at_compile() {
        assert!(ChatTemplate::new(Some("{% bogus".into()), None, None).is_err());
    }
}
