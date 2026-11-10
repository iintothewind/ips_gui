use crate::ips::types::{LoraInfo, PromptDetails};

pub fn extract_details(text: &str) -> PromptDetails {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return PromptDetails::default();
    }

    let (positive, negative, settings) = split_prompts_and_settings(trimmed);

    PromptDetails {
        model: parse_model(settings.unwrap_or(trimmed)),
        loras: parse_loras(trimmed),
        positive_prompt: non_empty(positive),
        negative_prompt: non_empty(negative),
    }
}

fn split_prompts_and_settings(text: &str) -> (&str, &str, Option<&str>) {
    let (body, settings) = match find_line_marker(text, "Steps:") {
        Some(idx) => (&text[..idx], Some(text[idx..].trim())),
        None => (text, None),
    };

    match body.find("Negative prompt:") {
        Some(idx) => {
            let positive = body[..idx].trim();
            let negative = body[idx + "Negative prompt:".len()..].trim();
            (positive, negative, settings)
        }
        None => {
            let positive = if body.trim_start().starts_with("Steps:") {
                ""
            } else {
                body.trim()
            };
            (positive, "", settings)
        }
    }
}

fn find_line_marker(text: &str, marker: &str) -> Option<usize> {
    if text.starts_with(marker) {
        return Some(0);
    }

    let needle = format!("\n{marker}");
    text.find(&needle).map(|idx| idx + 1)
}

/// Parse the checkpoint model name, avoiding false matches on "Model hash:".
fn parse_model(text: &str) -> Option<String> {
    for needle in [", Model:", "\nModel:", "Model:"] {
        let Some(idx) = text.find(needle) else {
            continue;
        };
        if needle == "Model:" && text[idx..].starts_with("Model hash:") {
            continue;
        }

        let rest = text[idx + needle.len()..].trim_start();
        if rest.starts_with('"') {
            let inner = &rest[1..];
            let end = inner.find('"')?;
            if let Some(model) = non_empty(inner[..end].trim()) {
                return Some(model);
            }
            continue;
        }

        let end = rest.find([',', '\n', '\r']).unwrap_or(rest.len());
        if let Some(model) = non_empty(rest[..end].trim()) {
            return Some(model);
        }
    }

    None
}

fn parse_loras(text: &str) -> Vec<LoraInfo> {
    let mut loras: Vec<LoraInfo> = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find("<lora:") {
        let after_start = &rest[start + "<lora:".len()..];
        let Some(end) = after_start.find('>') else {
            break;
        };
        let tag = &after_start[..end];
        let mut parts = tag.rsplitn(2, ':');
        let weight = parts.next().unwrap_or_default().trim();
        let name = parts.next().unwrap_or_default().trim();

        if !name.is_empty() && !weight.is_empty() && !loras.iter().any(|l| l.name == name) {
            loras.push(LoraInfo {
                name: name.to_string(),
                weight: weight.to_string(),
            });
        }

        rest = &after_start[end + 1..];
    }

    loras
}

fn non_empty(value: &str) -> Option<String> {
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

    #[test]
    fn extracts_positive_negative_model_and_loras() {
        let text = "masterpiece, <lora:detail:0.7>\nNegative prompt: blurry\nSteps: 30, Model: base.safetensors, Seed: 1";
        let details = extract_details(text);

        assert_eq!(
            details.positive_prompt.as_deref(),
            Some("masterpiece, <lora:detail:0.7>")
        );
        assert_eq!(details.negative_prompt.as_deref(), Some("blurry"));
        assert_eq!(details.model.as_deref(), Some("base.safetensors"));
        assert_eq!(details.loras[0].name, "detail");
        assert_eq!(details.loras[0].weight, "0.7");
    }

    #[test]
    fn model_stops_at_comma_not_later_quote() {
        let text = "pos\nNegative prompt: neg\nSteps: 20, Model: DuchaitenPonyReal_v20, Denoising strength: 0.6, Clip skip: 2, Lora hashes: \"abc\"";
        let details = extract_details(text);
        assert_eq!(details.model.as_deref(), Some("DuchaitenPonyReal_v20"));
    }

    #[test]
    fn model_supports_quoted_value() {
        let text = "pos\nSteps: 20, Model: \"PlantMilkModelSuite_almond.safetensors\", Seed: 1";
        let details = extract_details(text);
        assert_eq!(
            details.model.as_deref(),
            Some("PlantMilkModelSuite_almond.safetensors")
        );
    }

    #[test]
    fn model_skips_model_hash_field() {
        let text = "pos\nNegative prompt: neg\nSteps: 34, Model hash: 24f68a5155, Model: PlantMilkModelSuite_almond, Clip skip: 2";
        let details = extract_details(text);
        assert_eq!(
            details.model.as_deref(),
            Some("PlantMilkModelSuite_almond")
        );
    }

    #[test]
    fn leaves_missing_fields_empty() {
        let details = extract_details("Steps: 4, Sampler: Euler");
        assert!(details.positive_prompt.is_none());
        assert!(details.negative_prompt.is_none());
        assert!(details.model.is_none());
        assert!(details.loras.is_empty());
    }
}
