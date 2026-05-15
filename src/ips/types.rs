use std::path::PathBuf;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct PromptRecord {
    pub path: PathBuf,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loras: Vec<LoraInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub positive_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,
    pub generator: Generator,
    pub metadata_key: &'static str,
}

impl PromptRecord {
    pub fn with_details(
        path: PathBuf,
        prompt: String,
        generator: Generator,
        metadata_key: &'static str,
        details: PromptDetails,
    ) -> Self {
        Self {
            path,
            prompt,
            model: details.model,
            loras: details.loras,
            positive_prompt: details.positive_prompt,
            negative_prompt: details.negative_prompt,
            generator,
            metadata_key,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PromptDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loras: Vec<LoraInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub positive_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negative_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LoraInfo {
    pub name: String,
    pub weight: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Generator {
    A1111,
    ComfyUI,
    NovelAI,
    InvokeAI,
    Unknown,
}

impl std::fmt::Display for Generator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Generator::A1111 => write!(f, "a1111"),
            Generator::ComfyUI => write!(f, "comfyui"),
            Generator::NovelAI => write!(f, "novelai"),
            Generator::InvokeAI => write!(f, "invokeai"),
            Generator::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MatchResult {
    pub record: PromptRecord,
    pub score: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub query: String,
    pub path: PathBuf,
    pub match_mode: MatchMode,
    pub min_score: i64,
    pub depth: Option<usize>,
    pub no_recursive: bool,
    pub verbose: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatchMode {
    Exact,
    Fuzzy,
    Regex,
}
