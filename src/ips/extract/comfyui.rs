use serde_json::Value;
use std::collections::HashSet;

use crate::ips::types::{LoraInfo, PromptDetails};

pub fn extract_from_workflow(json: &Value) -> Vec<String> {
    let mut prompts = Vec::new();

    let nodes = match json.as_object() {
        Some(o) => o,
        None => return prompts,
    };

    for (_node_id, node) in nodes {
        let class_type = match node.get("class_type").and_then(|v| v.as_str()) {
            Some(ct) => ct,
            None => continue,
        };

        if !is_text_encode_node(class_type) {
            continue;
        }

        let inputs = match node.get("inputs").and_then(|v| v.as_object()) {
            Some(i) => i,
            None => continue,
        };

        for field in &["text_g", "text_l", "text", "prompt", "neg", "negative_prompt"] {
            if let Some(text) = inputs.get(*field).and_then(|v| v.as_str()) {
                let text = text.trim().to_string();
                if !text.is_empty() && !prompts.contains(&text) {
                    prompts.push(text);
                }
            }
        }
    }

    prompts
}

pub fn extract_details_from_workflow(json: &Value) -> PromptDetails {
    let Some(nodes) = json.as_object() else {
        return PromptDetails::default();
    };

    let mut details = PromptDetails {
        model: extract_model(json),
        loras: extract_loras(json),
        positive_prompt: None,
        negative_prompt: None,
    };

    for (_node_id, node) in nodes {
        let class_type = node.get("class_type").and_then(|v| v.as_str()).unwrap_or_default();
        if !is_sampler_node(class_type) && class_type != "CFGGuider" {
            continue;
        }

        let Some(inputs) = node.get("inputs").and_then(|v| v.as_object()) else {
            continue;
        };

        if details.positive_prompt.is_none() {
            details.positive_prompt = inputs
                .get("positive")
                .and_then(ref_cond)
                .and_then(|cond| resolve_conditioning_text(json, &cond, PromptRole::Positive));
        }
        if details.negative_prompt.is_none() {
            details.negative_prompt = inputs
                .get("negative")
                .and_then(ref_cond)
                .and_then(|cond| resolve_conditioning_text(json, &cond, PromptRole::Negative));
        }

        if details.positive_prompt.is_some() || details.negative_prompt.is_some() {
            break;
        }
    }

    details
}

fn extract_model(json: &Value) -> Option<String> {
    let nodes = json.as_object()?;

    for field in ["ckpt_name", "unet_name"] {
        for (_node_id, node) in nodes {
            let class_type = node.get("class_type").and_then(|v| v.as_str()).unwrap_or_default();
            if !is_generation_model_node(class_type) {
                continue;
            }

            if let Some(model) = node
                .get("inputs")
                .and_then(|v| v.get(field))
                .and_then(value_to_string)
                .and_then(non_empty)
            {
                return Some(model);
            }
        }
    }

    None
}

fn is_generation_model_node(class_type: &str) -> bool {
    matches!(
        class_type,
        "CheckpointLoaderSimple"
            | "CheckpointLoader"
            | "CheckpointLoaderNF4"
            | "UNETLoader"
            | "UnetLoaderGGUF"
    )
}

fn extract_loras(json: &Value) -> Vec<LoraInfo> {
    let Some(nodes) = json.as_object() else {
        return Vec::new();
    };

    let mut loras = Vec::new();

    for (_node_id, node) in nodes {
        let class_type = node.get("class_type").and_then(|v| v.as_str()).unwrap_or_default();
        if !class_type.to_lowercase().contains("lora") {
            continue;
        }

        let Some(inputs) = node.get("inputs").and_then(|v| v.as_object()) else {
            continue;
        };

        if let Some(name) = inputs
            .get("lora_name")
            .and_then(value_to_string)
            .and_then(non_empty)
        {
            let model_weight = inputs.get("strength_model").and_then(value_to_string);
            let clip_weight = inputs.get("strength_clip").and_then(value_to_string);
            let weight = format_lora_weight(model_weight.as_deref(), clip_weight.as_deref())
                .unwrap_or_else(|| "1".to_string());
            push_lora(&mut loras, name, weight);
        }

        for (_field, value) in inputs {
            let Some(value) = value.as_object() else {
                continue;
            };
            if matches!(value.get("on").and_then(|v| v.as_bool()), Some(false)) {
                continue;
            }
            let Some(name) = value
                .get("lora")
                .and_then(value_to_string)
                .and_then(non_empty)
            else {
                continue;
            };
            let weight = value
                .get("strength")
                .and_then(value_to_string)
                .or_else(|| value.get("strength_model").and_then(value_to_string))
                .unwrap_or_else(|| "1".to_string());
            push_lora(&mut loras, name, weight);
        }
    }

    loras
}

fn push_lora(loras: &mut Vec<LoraInfo>, name: String, weight: String) {
    if loras.iter().any(|l| l.name == name) {
        return;
    }
    loras.push(LoraInfo { name, weight });
}

fn format_lora_weight(model_weight: Option<&str>, clip_weight: Option<&str>) -> Option<String> {
    match (model_weight, clip_weight) {
        (Some(model), Some(clip)) if model == clip => Some(model.to_string()),
        (Some(model), Some(clip)) => Some(format!("{model} / {clip}")),
        (Some(model), None) => Some(model.to_string()),
        (None, Some(clip)) => Some(clip.to_string()),
        (None, None) => None,
    }
}

struct CondRef {
    id: String,
    index: u64,
}

#[derive(Clone, Copy)]
enum PromptRole {
    Positive,
    Negative,
}

fn resolve_conditioning_text(json: &Value, cond: &CondRef, role: PromptRole) -> Option<String> {
    let mut visited = HashSet::new();
    resolve_conditioning_text_inner(json, cond, role, &mut visited)
}

fn resolve_conditioning_text_inner(
    json: &Value,
    cond: &CondRef,
    role: PromptRole,
    visited: &mut HashSet<(String, u64)>,
) -> Option<String> {
    if !visited.insert((cond.id.clone(), cond.index)) {
        return None;
    }

    let node = json.as_object()?.get(&cond.id)?;
    let class_type = node.get("class_type").and_then(|v| v.as_str()).unwrap_or_default();
    if class_type.contains("ZeroOut") {
        return None;
    }

    // Output slot 1 of a conditioning node is the negative stream: its text comes from the
    // node's negative field, never from the shared positive field.
    let negative_stream = cond.index > 0;
    let role = if negative_stream {
        PromptRole::Negative
    } else {
        role
    };

    if is_text_encode_node(class_type) {
        if let Some(text) = extract_text_from_node(node, text_fields_for(role, negative_stream)) {
            return Some(text);
        }
    }

    let inputs = node.get("inputs").and_then(|v| v.as_object())?;
    for &field in input_fields_for(role) {
        let Some(next) = inputs.get(field).and_then(ref_cond) else {
            continue;
        };
        let next_role = field_role(field).unwrap_or(role);
        if let Some(text) = resolve_conditioning_text_inner(json, &next, next_role, visited) {
            return Some(text);
        }
    }

    None
}

fn text_fields_for(role: PromptRole, negative_stream: bool) -> &'static [&'static str] {
    match (role, negative_stream) {
        (PromptRole::Positive, _) => &["text_g", "text_l", "text", "prompt", "positive"],
        (PromptRole::Negative, true) => &["neg", "negative_prompt", "negative"],
        (PromptRole::Negative, false) => &["neg", "negative_prompt", "negative", "text", "prompt"],
    }
}

fn input_fields_for(role: PromptRole) -> &'static [&'static str] {
    match role {
        PromptRole::Positive => &["positive", "conditioning", "conditioning1", "conditioning2"],
        PromptRole::Negative => &[
            "negative",
            "neg",
            "conditioning",
            "conditioning1",
            "conditioning2",
        ],
    }
}

fn field_role(field: &str) -> Option<PromptRole> {
    match field {
        "positive" | "positive_prompt" => Some(PromptRole::Positive),
        "negative" | "negative_prompt" | "neg" => Some(PromptRole::Negative),
        _ => None,
    }
}

fn extract_text_from_node(node: &Value, fields: &[&str]) -> Option<String> {
    let inputs = node.get("inputs").and_then(|v| v.as_object())?;
    let mut parts = Vec::new();

    for field in fields {
        if let Some(text) = inputs
            .get(*field)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            if !parts.contains(&text) {
                parts.push(text);
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn is_sampler_node(class_type: &str) -> bool {
    class_type.contains("KSampler")
}

fn is_text_encode_node(class_type: &str) -> bool {
    matches!(
        class_type,
        "CLIPTextEncode"
            | "CLIPTextEncodeSDXL"
            | "CLIPTextEncodeFlux"
            | "TextEncodeQwenImageEdit"
            | "TextEncodeQwenImageEditPlus"
    ) || class_type.contains("Prompt")
        || class_type.contains("TextEncode")
}

fn ref_cond(value: &Value) -> Option<CondRef> {
    let array = value.as_array()?;
    let id = array.first().and_then(value_to_string).and_then(non_empty)?;
    let index = array.get(1).and_then(|v| v.as_u64()).unwrap_or(0);
    Some(CondRef { id, index })
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_clip_text_encode() {
        let workflow = json!({
            "1": {
                "class_type": "CLIPTextEncode",
                "inputs": { "text": "a beautiful sunset over the ocean" }
            },
            "2": {
                "class_type": "CLIPTextEncode",
                "inputs": { "text": "ugly, blurry, low quality" }
            }
        });
        let prompts = extract_from_workflow(&workflow);
        assert_eq!(prompts.len(), 2);
        assert!(prompts.contains(&"a beautiful sunset over the ocean".to_string()));
        assert!(prompts.contains(&"ugly, blurry, low quality".to_string()));
    }

    #[test]
    fn extracts_sdxl_text_g_and_l() {
        let workflow = json!({
            "3": {
                "class_type": "CLIPTextEncodeSDXL",
                "inputs": {
                    "text_g": "a detailed landscape",
                    "text_l": "soft lighting, 4k"
                }
            }
        });
        let prompts = extract_from_workflow(&workflow);
        assert_eq!(prompts.len(), 2);
    }

    #[test]
    fn deduplicates_identical_prompts() {
        let workflow = json!({
            "1": {
                "class_type": "CLIPTextEncode",
                "inputs": { "text": "same prompt" }
            },
            "2": {
                "class_type": "CLIPTextEncode",
                "inputs": { "text": "same prompt" }
            }
        });
        let prompts = extract_from_workflow(&workflow);
        assert_eq!(prompts.len(), 1);
    }

    #[test]
    fn ignores_non_text_nodes() {
        let workflow = json!({
            "1": {
                "class_type": "KSampler",
                "inputs": { "steps": 20, "cfg": 7.0 }
            }
        });
        let prompts = extract_from_workflow(&workflow);
        assert!(prompts.is_empty());
    }

    #[test]
    fn extracts_qwen_prompt_field() {
        let workflow = json!({
            "1": {
                "class_type": "TextEncodeQwenImageEditPlus",
                "inputs": { "prompt": "edit prompt text" }
            }
        });

        let prompts = extract_from_workflow(&workflow);
        assert_eq!(prompts, vec!["edit prompt text".to_string()]);
    }

    #[test]
    fn extracts_structured_details_from_ksampler() {
        let workflow = json!({
            "1": {
                "class_type": "CheckpointLoaderSimple",
                "inputs": { "ckpt_name": "model.safetensors" }
            },
            "2": {
                "class_type": "LoraLoader",
                "inputs": {
                    "lora_name": "style.safetensors",
                    "strength_model": 0.7,
                    "strength_clip": 0.7
                }
            },
            "3": {
                "class_type": "CLIPTextEncode",
                "inputs": { "text": "positive text" }
            },
            "4": {
                "class_type": "CLIPTextEncode",
                "inputs": { "text": "negative text" }
            },
            "5": {
                "class_type": "KSampler",
                "inputs": {
                    "positive": ["3", 0],
                    "negative": ["4", 0]
                }
            }
        });

        let details = extract_details_from_workflow(&workflow);
        assert_eq!(details.model.as_deref(), Some("model.safetensors"));
        assert_eq!(details.loras[0].name, "style.safetensors");
        assert_eq!(details.loras[0].weight, "0.7");
        assert_eq!(details.positive_prompt.as_deref(), Some("positive text"));
        assert_eq!(details.negative_prompt.as_deref(), Some("negative text"));
    }

    #[test]
    fn extracts_enabled_rgthree_loras() {
        let workflow = json!({
            "1": {
                "class_type": "Power Lora Loader (rgthree)",
                "inputs": {
                    "lora_1": { "on": false, "lora": "off.safetensors", "strength": 0.3 },
                    "lora_2": { "on": true, "lora": "on.safetensors", "strength": 0.5 }
                }
            }
        });

        let details = extract_details_from_workflow(&workflow);
        assert_eq!(details.loras.len(), 1);
        assert_eq!(details.loras[0].name, "on.safetensors");
        assert_eq!(details.loras[0].weight, "0.5");
    }

    #[test]
    fn shared_encode_node_negative_output_is_not_the_positive_prompt() {
        let workflow = json!({
            "1": {
                "class_type": "TextEncodeQwenImage21",
                "inputs": { "prompt": "a woman in a floral dress", "negative_prompt": "" }
            },
            "2": {
                "class_type": "KSampler",
                "inputs": { "positive": ["1", 0], "negative": ["1", 1] }
            }
        });

        let details = extract_details_from_workflow(&workflow);
        assert_eq!(
            details.positive_prompt.as_deref(),
            Some("a woman in a floral dress")
        );
        assert_eq!(details.negative_prompt, None);
    }

    #[test]
    fn shared_encode_node_negative_output_text() {
        let workflow = json!({
            "1": {
                "class_type": "TextEncodeQwenImage21",
                "inputs": { "prompt": "portrait", "negative_prompt": "blurry, low quality" }
            },
            "2": {
                "class_type": "CFGGuider",
                "inputs": { "positive": ["1", 0], "negative": ["1", 1] }
            }
        });

        let details = extract_details_from_workflow(&workflow);
        assert_eq!(details.positive_prompt.as_deref(), Some("portrait"));
        assert_eq!(
            details.negative_prompt.as_deref(),
            Some("blurry, low quality")
        );
    }

    #[test]
    fn zero_out_negative_stream_has_no_negative_prompt() {
        let workflow = json!({
            "1": { "class_type": "CLIPTextEncode", "inputs": { "text": "positive text" } },
            "2": {
                "class_type": "ConditioningZeroOut",
                "inputs": { "conditioning": ["1", 0] }
            },
            "3": {
                "class_type": "KSampler",
                "inputs": { "positive": ["1", 0], "negative": ["2", 0] }
            }
        });

        let details = extract_details_from_workflow(&workflow);
        assert_eq!(details.positive_prompt.as_deref(), Some("positive text"));
        assert_eq!(details.negative_prompt, None);
    }

    #[test]
    fn propagates_negative_role_through_conditioning_chain() {
        let workflow = json!({
            "1": { "class_type": "CLIPTextEncode", "inputs": { "text": "negative text" } },
            "2": {
                "class_type": "Conditioning (Set Strength)",
                "inputs": { "strength": 1.0, "conditioning": ["1", 0] }
            },
            "3": { "class_type": "CLIPTextEncode", "inputs": { "text": "positive text" } },
            "4": {
                "class_type": "KSampler",
                "inputs": { "positive": ["3", 0], "negative": ["2", 0] }
            }
        });

        let details = extract_details_from_workflow(&workflow);
        assert_eq!(details.positive_prompt.as_deref(), Some("positive text"));
        assert_eq!(details.negative_prompt.as_deref(), Some("negative text"));
    }

    #[test]
    fn qwen_encode_used_as_negative_keeps_its_prompt_field() {
        let workflow = json!({
            "1": {
                "class_type": "TextEncodeQwenImageEditPlus",
                "inputs": { "prompt": "blurry, deformed" }
            },
            "2": {
                "class_type": "KSampler",
                "inputs": { "positive": ["3", 0], "negative": ["1", 0] }
            },
            "3": { "class_type": "CLIPTextEncode", "inputs": { "text": "positive text" } }
        });

        let details = extract_details_from_workflow(&workflow);
        assert_eq!(details.positive_prompt.as_deref(), Some("positive text"));
        assert_eq!(details.negative_prompt.as_deref(), Some("blurry, deformed"));
    }

    #[test]
    fn qwen21_shared_encode_node_fixture() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/qwen21_shared_encode.json"
        ))
        .unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();

        let details = extract_details_from_workflow(&json);
        assert_eq!(
            details.model.as_deref(),
            Some("qwen_image_2.1_turbo_int8_convrot.safetensors")
        );
        assert_eq!(
            details.positive_prompt.as_deref(),
            Some("portrait of a woman in a floral dress, cinematic lighting")
        );
        assert_eq!(
            details.negative_prompt, None,
            "conditioning output slot 1 must not reuse the shared positive prompt"
        );
        assert_eq!(details.loras.len(), 1);
        assert_eq!(details.loras[0].name, "qwen\\style_qwen21.safetensors");

        assert_eq!(
            extract_from_workflow(&json),
            vec!["portrait of a woman in a floral dress, cinematic lighting".to_string()]
        );
    }

}
