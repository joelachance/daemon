use aws_config;
use aws_sdk_bedrockruntime::types::{ContentBlock, ConversationRole, Message, SystemContentBlock};
use aws_sdk_bedrockruntime::Client;
use aws_types::region::Region;
use serde::Deserialize;
use std::env;

const DEFAULT_MODEL_ID: &str = "amazon.nova-micro-v1:0";
const DEFAULT_REGION: &str = "us-west-2";

#[derive(Debug, Deserialize, Default)]
pub struct BedrockPlan {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
}

pub struct BedrockClient {
    client: Client,
    model_id: String,
}

impl BedrockClient {
    pub async fn new() -> Result<Self, String> {
        let config = aws_config::from_env()
            .region(Region::new(DEFAULT_REGION))
            .load()
            .await;
        let client = Client::new(&config);
        let model_id = env::var("GG_BEDROCK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL_ID.to_string());
        Ok(Self { client, model_id })
    }

    pub async fn plan_git_commands(
        &self,
        prompt: &str,
        repo_root: Option<&str>,
    ) -> Result<BedrockPlan, String> {
        let system_prompt = build_system_prompt();
        let user_prompt = build_user_prompt(prompt, repo_root);

        let message = Message::builder()
            .role(ConversationRole::User)
            .content(ContentBlock::Text(user_prompt))
            .build()
            .map_err(|err| err.to_string())?;

        let response = self
            .client
            .converse()
            .model_id(&self.model_id)
            .system(SystemContentBlock::Text(system_prompt))
            .messages(message)
            .send()
            .await
            .map_err(|err| err.to_string())?;

        let output = response
            .output()
            .and_then(|output| output.as_message().ok())
            .ok_or_else(|| "bedrock: missing output message".to_string())?;

        let text = extract_text(output.content());
        if text.trim().is_empty() {
            return Err("bedrock: empty response".to_string());
        }

        parse_plan(&text)
    }
}

fn build_system_prompt() -> String {
    "You are a git planning assistant. Output only a JSON object with keys: summary, commands, assumptions, risks. commands must be an array of git CLI commands only. Do not include shell operators, pipes, redirections, or quotes. Do not include explanations outside JSON."
        .to_string()
}

fn build_user_prompt(prompt: &str, repo_root: Option<&str>) -> String {
    match repo_root {
        Some(root) => format!(
            "Prompt: {prompt}\nRepo root: {root}\nGenerate a git command plan.",
        ),
        None => format!("Prompt: {prompt}\nGenerate a git command plan."),
    }
}

fn extract_text(blocks: &[ContentBlock]) -> String {
    let mut text = String::new();
    for block in blocks {
        if let ContentBlock::Text(value) = block {
            text.push_str(value);
        }
    }
    text
}

fn parse_plan(text: &str) -> Result<BedrockPlan, String> {
    let trimmed = text.trim();
    let cleaned = strip_code_fences(trimmed);
    if let Ok(plan) = serde_json::from_str::<BedrockPlan>(cleaned) {
        return Ok(plan);
    }

    let start = cleaned.find('{');
    let end = cleaned.rfind('}');
    match (start, end) {
        (Some(start), Some(end)) if end > start => {
            let slice = &cleaned[start..=end];
            serde_json::from_str::<BedrockPlan>(slice)
                .map_err(|err| format!("bedrock: invalid json ({err})"))
        }
        _ => Err("bedrock: could not find json object".to_string()),
    }
}

fn strip_code_fences(text: &str) -> &str {
    let trimmed = text.trim();
    if trimmed.starts_with("```") {
        let without_start = trimmed.trim_start_matches("```");
        let without_lang = without_start.trim_start_matches("json").trim_start();
        if let Some(end) = without_lang.rfind("```") {
            return without_lang[..end].trim();
        }
    }
    trimmed
}
