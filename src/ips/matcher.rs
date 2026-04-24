use std::cell::RefCell;
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use crate::ips::types::{Config, MatchMode, MatchResult, PromptRecord};

thread_local! {
    static SKIM: SkimMatcherV2 = SkimMatcherV2::default();
    static REGEX_CACHE: RefCell<Option<(String, regex::Regex)>> = RefCell::new(None);
}

pub fn match_record(record: &PromptRecord, config: &Config) -> Option<MatchResult> {
    match config.match_mode {
        MatchMode::Exact => match_exact(record, config),
        MatchMode::Fuzzy => match_fuzzy(record, config),
        MatchMode::Regex => match_regex(record, config),
    }
}

fn match_exact(record: &PromptRecord, config: &Config) -> Option<MatchResult> {
    let query_lower = config.query.to_lowercase();
    let prompt_lower = record.prompt.to_lowercase();

    prompt_lower.find(&query_lower).map(|_| MatchResult {
        record: record.clone(),
        score: None,
    })
}

fn match_fuzzy(record: &PromptRecord, config: &Config) -> Option<MatchResult> {
    SKIM.with(|matcher| {
        let (score, _) = matcher.fuzzy_indices(&record.prompt, &config.query)?;
        if score < config.min_score {
            return None;
        }
        Some(MatchResult {
            record: record.clone(),
            score: Some(score),
        })
    })
}

fn match_regex(record: &PromptRecord, config: &Config) -> Option<MatchResult> {
    REGEX_CACHE.with(|cache| {
        let mut c = cache.borrow_mut();
        if c.as_ref().map_or(true, |(q, _)| q != &config.query) {
            let re = regex::Regex::new(&config.query).ok()?;
            *c = Some((config.query.clone(), re));
        }
        let matched = c.as_ref()?.1.find(&record.prompt).is_some();
        drop(c);
        if matched {
            Some(MatchResult { record: record.clone(), score: None })
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use crate::ips::types::Generator;

    fn make_record(prompt: &str) -> PromptRecord {
        PromptRecord {
            path: PathBuf::from("test.png"),
            prompt: prompt.to_string(),
            generator: Generator::Unknown,
            metadata_key: "parameters",
        }
    }

    fn config(query: &str, mode: MatchMode) -> Config {
        Config {
            query: query.to_string(),
            path: PathBuf::from("."),
            match_mode: mode,
            min_score: 50,
            depth: None,
            no_recursive: false,
            verbose: false,
        }
    }

    #[test]
    fn exact_match_found() {
        let record = make_record("masterpiece, 1girl, cyberpunk");
        let result = match_exact(&record, &config("cyberpunk", MatchMode::Exact));
        assert!(result.is_some());
    }

    #[test]
    fn exact_match_case_insensitive() {
        let record = make_record("Masterpiece, 1girl");
        let result = match_exact(&record, &config("masterpiece", MatchMode::Exact));
        assert!(result.is_some());
    }

    #[test]
    fn exact_match_not_found() {
        let record = make_record("landscape, mountains");
        let result = match_exact(&record, &config("cyberpunk", MatchMode::Exact));
        assert!(result.is_none());
    }

    #[test]
    fn fuzzy_match_found() {
        let record = make_record("a beautiful cyberpunk cityscape");
        let result = match_fuzzy(&record, &config("cyber", MatchMode::Fuzzy));
        assert!(result.is_some());
    }

    #[test]
    fn regex_match_found() {
        let record = make_record("1girl, masterpiece, detailed background");
        let result = match_regex(&record, &config(r"\d+girl", MatchMode::Regex));
        assert!(result.is_some());
    }

    #[test]
    fn regex_no_match() {
        let record = make_record("landscape, trees");
        let result = match_regex(&record, &config(r"\d+girl", MatchMode::Regex));
        assert!(result.is_none());
    }
}
