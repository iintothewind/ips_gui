use serde_json::Value;

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

        for field in &["text_g", "text_l", "text"] {
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

fn is_text_encode_node(class_type: &str) -> bool {
    matches!(
        class_type,
        "CLIPTextEncode" | "CLIPTextEncodeSDXL" | "CLIPTextEncodeFlux"
    ) || class_type.contains("Prompt")
        || class_type.contains("TextEncode")
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
}
