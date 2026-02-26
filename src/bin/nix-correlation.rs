use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use rusqlite::Connection;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn main() -> Result<()> {
    let mut conn = Connection::open(format!("data/tags_alterations_full_2025-10_v2.db"))?;
    // let mut conn = Connection::open(format!("data/tags_alterations_teaser_2025-05.db"))?;
    let table_exists = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='tag_inconsistencies'")
        .and_then(|mut stmt| stmt.exists([]))
        .unwrap_or(false);

    if !table_exists {
        println!("Table doesn't exist");
        return Ok(());
    }

    println!("Loading origin URLs from database...");
    let mut stmt = conn.prepare("SELECT DISTINCT origin_url FROM tag_inconsistencies")?;
    let origins: Vec<String> = stmt
        .query_map([], |row| Ok(row.get(0)?))?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    println!("Found {} unique origins", origins.len());

    let nixpkgs_path = "/swh/scratch/rapaport/nixpkgs";
    println!("Searching for .nix files in {}...", nixpkgs_path);

    println!("Collecting .nix file paths...");
    let nix_files = collect_nix_files(Path::new(nixpkgs_path))?;
    println!("Found {} .nix files to process", nix_files.len());

    let pb = ProgressBar::new(nix_files.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} files ({eta})")
            .unwrap()
            .progress_chars("##-"),
    );

    let origins_arc = Arc::new(origins);
    
    let results: Vec<(String, String)> = nix_files
        .par_iter()
        .flat_map(|file_path| {
            let matches = check_nix_file_parallel(file_path, &origins_arc);
            pb.inc(1);
            matches
        })
        .collect();

    pb.finish_with_message("Done scanning files");

    // Build HashMap from results
    let mut url_to_files: HashMap<String, Vec<String>> = HashMap::new();
    for (origin, file_path) in results {
        url_to_files
            .entry(origin)
            .or_insert_with(Vec::new)
            .push(file_path);
    }

    println!("\nFound {} origins referenced in Nix files", url_to_files.len());

    // Create table and store results
    println!("Storing results in database...");
    let total_entries = store_results(&mut conn, &url_to_files)?;
    println!("Stored {} correlation entries", total_entries);

    // Print summary
    println!("\nTop 10 most referenced origins:");
    let mut sorted: Vec<_> = url_to_files.iter().collect();
    sorted.sort_by_key(|(_, files)| std::cmp::Reverse(files.len()));
    for (url, files) in sorted.iter().take(10) {
        println!("  {} -> {} file(s)", url, files.len());
    }

    Ok(())
}

fn collect_nix_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    if !dir.is_dir() {
        return Ok(files);
    }

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            files.extend(collect_nix_files(&path)?);
        } else if path.extension().and_then(|s| s.to_str()) == Some("nix") {
            files.push(path);
        }
    }

    Ok(files)
}

fn check_nix_file_parallel(
    file_path: &PathBuf,
    origins: &Arc<Vec<String>>,
) -> Vec<(String, String)> {
    let mut matches = Vec::new();
    
    let content = match fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(_) => return matches, // Skip files that can't be read
    };
    
    let file_path_str = file_path.to_string_lossy().to_string();

    // Extract URLs from fetch* functions and src attributes
    let urls_in_file = extract_urls_from_nix(&content);
    
    // Also extract reconstructed URLs from owner/repo pairs
    let reconstructed_urls = extract_reconstructed_urls(&content);
    
    for origin in origins.iter() {
        if url_matches_any(&urls_in_file, origin) || url_matches_any(&reconstructed_urls, origin) {
            matches.push((origin.clone(), file_path_str.clone()));
        }
    }

    matches
}

fn extract_urls_from_nix(content: &str) -> Vec<String> {
    let mut urls = Vec::new();

    // Pattern 1: url = "..."; or url="...";
    for line in content.lines() {
        if let Some(url) = extract_url_value(line, "url") {
            // Only add if it looks like an actual URL
            if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("git://") {
                urls.push(url);
            }
        }
    }

    // Pattern 2: URLs in common fetch functions
    // fetchFromGitHub, fetchFromGitLab, fetchgit, etc.
    // These often have owner/repo or just url fields

    // Simple approach: extract any quoted string that looks like a URL
    let url_pattern = regex::Regex::new(r#"(?:https?://|git://)[^\s"']+(?:\.git)?"#).unwrap();
    for capture in url_pattern.find_iter(content) {
        let url = capture.as_str().to_string();
        // Only include if it's within a fetch context or src attribute
        if is_in_fetch_context(content, capture.start()) {
            urls.push(url);
        }
    }

    urls
}

fn extract_reconstructed_urls(content: &str) -> Vec<String> {
    let mut urls = Vec::new();
    
    // Map fetch functions to their base URLs
    let fetch_patterns = [
        ("fetchFromGitHub", "https://github.com"),
        ("fetchFromGitLab", "https://gitlab.com"),
        ("fetchFromGitea", "https://gitea.com"),
        ("fetchFromGitiles", "https://gerrit.googlesource.com"),
        ("fetchFromSourcehut", "https://git.sr.ht"),
        ("fetchFromBitbucket", "https://bitbucket.org"),
    ];
    
    for (fetch_func, base_url) in &fetch_patterns {
        // Find all occurrences of this fetch function
        let mut start = 0;
        while let Some(pos) = content[start..].find(fetch_func) {
            let abs_pos = start + pos;
            start = abs_pos + 1;
            
            // Extract the content after the fetch function (looking for the opening brace)
            let after_fetch = &content[abs_pos + fetch_func.len()..];
            if let Some(brace_pos) = after_fetch.find('{') {
                // Find the closing brace (simplified - just look ahead ~1000 chars)
                let block_start = abs_pos + fetch_func.len() + brace_pos;
                let mut search_end = (block_start + 1000).min(content.len());
                
                // Ensure we're on a character boundary
                while search_end < content.len() && !content.is_char_boundary(search_end) {
                    search_end += 1;
                }
                
                let block = &content[block_start..search_end];
                
                // Extract owner and repo (or pname)
                let owner = extract_attribute_value(block, "owner");
                let repo = extract_attribute_value(block, "repo")
                    .or_else(|| extract_attribute_value(block, "pname"));
                
                if let (Some(owner_val), Some(repo_val)) = (owner, repo) {
                    let reconstructed_url = format!("{}/{}/{}", base_url, owner_val, repo_val);
                    urls.push(reconstructed_url);
                }
            }
        }
    }
    
    urls
}

fn extract_attribute_value(block: &str, attr: &str) -> Option<String> {
    // Look for patterns like: owner = "value"; or owner="value";
    for line in block.lines() {
        let trimmed = line.trim();
        
        // Try with spaces around =
        if let Some(idx) = trimmed.find(&format!("{} =", attr)) {
            let after_eq = &trimmed[idx + attr.len() + 2..].trim_start();
            if let Some(val) = extract_quoted_value(after_eq) {
                return Some(val);
            }
        }
        
        // Try without spaces
        if let Some(idx) = trimmed.find(&format!("{}=", attr)) {
            let after_eq = &trimmed[idx + attr.len() + 1..].trim_start();
            if let Some(val) = extract_quoted_value(after_eq) {
                return Some(val);
            }
        }
    }
    None
}

fn extract_quoted_value(s: &str) -> Option<String> {
    let s = s.trim();
    if s.starts_with('"') {
        if let Some(end_quote) = s[1..].find('"') {
            return Some(s[1..1 + end_quote].to_string());
        }
    }
    None
}

fn extract_url_value(line: &str, key: &str) -> Option<String> {
    let line = line.trim();
    if let Some(idx) = line.find(&format!("{}=", key)) {
        let after_eq = &line[idx + key.len() + 1..];
        let after_eq = after_eq.trim();
        if after_eq.starts_with('"') {
            if let Some(end_quote) = after_eq[1..].find('"') {
                return Some(after_eq[1..1 + end_quote].to_string());
            }
        }
    } else if let Some(idx) = line.find(&format!("{} =", key)) {
        let after_eq = &line[idx + key.len() + 2..];
        let after_eq = after_eq.trim();
        if after_eq.starts_with('"') {
            if let Some(end_quote) = after_eq[1..].find('"') {
                return Some(after_eq[1..1 + end_quote].to_string());
            }
        }
    }
    None
}

fn is_in_fetch_context(content: &str, pos: usize) -> bool {
    // Look backwards from position to find if we're in a fetch* function call
    let before = &content[..pos];
    
    // Get last ~500 chars, ensuring we slice on character boundaries
    let last_chars = if before.len() > 500 {
        // Find a safe character boundary by skipping back through the string
        let mut safe_start = before.len().saturating_sub(500);
        while safe_start > 0 && !before.is_char_boundary(safe_start) {
            safe_start -= 1;
        }
        &before[safe_start..]
    } else {
        before
    };

    // Check if we're inside a fetch* call or src = ... assignment
    let fetch_keywords = [
        "fetchFromGitHub", "fetchFromGitLab", "fetchFromGitea",
        "fetchgit", "fetchGit", "fetchurl", "src =", "src="
    ];

    for keyword in &fetch_keywords {
        if last_chars.contains(keyword) {
            return true;
        }
    }

    false
}

fn url_matches_any(urls_in_file: &[String], origin: &str) -> bool {
    let normalized_origin = normalize_url(origin);
    
    // Skip empty origins
    if normalized_origin.is_empty() {
        return false;
    }

    for url in urls_in_file {
        let normalized_url = normalize_url(url);
        
        // Skip empty URLs
        if normalized_url.is_empty() {
            continue;
        }
        
        // Only match if the URLs are exactly the same after normalization
        // (normalization already handles .git suffix, trailing slashes, http/https)
        if normalized_url == normalized_origin {
            return true;
        }
    }

    false
}

fn normalize_url(url: &str) -> String {
    url.trim()
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .replace("http://", "https://")
        .to_lowercase()
}

fn store_results(conn: &mut Connection, url_to_files: &HashMap<String, Vec<String>>) -> Result<usize> {
    conn.execute("DROP TABLE IF EXISTS nix_correlations", [])?;
    conn.execute(
        "CREATE TABLE nix_correlations (
            origin_url TEXT NOT NULL,
            nix_file_path TEXT NOT NULL,
            PRIMARY KEY (origin_url, nix_file_path)
        )",
        [],
    )?;

    let tx = conn.transaction()?;
    let mut stmt = tx.prepare(
        "INSERT INTO nix_correlations (origin_url, nix_file_path) VALUES (?1, ?2)",
    )?;

    let mut count = 0;
    for (url, files) in url_to_files {
        for file in files {
            stmt.execute(rusqlite::params![url, file])?;
            count += 1;
        }
    }

    drop(stmt);
    tx.commit()?;

    Ok(count)
}
