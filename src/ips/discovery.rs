use std::path::PathBuf;
use walkdir::WalkDir;
use crate::ips::types::Config;

static IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp"];

pub fn discover_files(config: &Config) -> Vec<PathBuf> {
    let mut walker = WalkDir::new(&config.path).follow_links(false);

    if config.no_recursive {
        walker = walker.max_depth(1);
    } else if let Some(depth) = config.depth {
        walker = walker.max_depth(depth);
    }

    walker
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(e) => {
                if e.file_type().is_file() && is_image(e.path()) {
                    Some(e.path().to_path_buf())
                } else {
                    None
                }
            }
            Err(e) => {
                if config.verbose {
                    eprintln!("ips: directory error: {}", e);
                }
                None
            }
        })
        .collect()
}

pub(crate) fn is_image(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| IMAGE_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use crate::ips::types::MatchMode;

    fn make_temp_tree() -> TempDir {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("a.png"), b"").unwrap();
        fs::write(root.join("b.jpg"), b"").unwrap();
        fs::write(root.join("ignore.txt"), b"").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("c.webp"), b"").unwrap();
        dir
    }

    #[test]
    fn finds_image_files_recursively() {
        let dir = make_temp_tree();
        let config = Config {
            query: String::new(),
            path: dir.path().to_path_buf(),
            match_mode: MatchMode::Exact,
            min_score: 50,
            depth: None,
            no_recursive: false,
            verbose: false,
        };
        let files = discover_files(&config);
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn respects_no_recursive() {
        let dir = make_temp_tree();
        let config = Config {
            query: String::new(),
            path: dir.path().to_path_buf(),
            match_mode: MatchMode::Exact,
            min_score: 50,
            depth: None,
            no_recursive: true,
            verbose: false,
        };
        let files = discover_files(&config);
        assert_eq!(files.len(), 2);
    }
}
