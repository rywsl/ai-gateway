use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct Finding {
    path: String,
    line: usize,
    text: String,
}

fn main() {
    let root = env::args().nth(1).unwrap_or_else(|| "crates".to_string());
    let baseline = match load_baseline("scripts/check-tokio-spawn.baseline") {
        Ok(v) => v,
        Err(err) => {
            eprintln!("check-tokio-spawn: {err}");
            std::process::exit(2);
        }
    };

    let mut findings = Vec::new();
    if let Err(err) = scan_dir(Path::new(&root), &mut findings) {
        eprintln!("check-tokio-spawn: {err}");
        std::process::exit(2);
    }
    findings.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));

    let mut unknown = 0usize;
    for finding in &findings {
        let key = format!("{}:{}:tokio-spawn", finding.path, finding.line);
        if baseline.contains(&key) {
            continue;
        }
        eprintln!(
            "{}:{}:1: tokio-spawn: bare tokio spawn must use gw_runtime::Supervisor or carry an reviewed baseline ({})",
            finding.path,
            finding.line,
            finding.text.trim()
        );
        unknown += 1;
    }

    if unknown > 0 {
        eprintln!("check-tokio-spawn: FAIL: {unknown} new finding(s)");
        std::process::exit(1);
    }
    println!(
        "check-tokio-spawn: OK ({} current baseline finding(s), no new findings)",
        findings.len()
    );
}

fn scan_dir(dir: &Path, out: &mut Vec<Finding>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if matches!(name.as_ref(), "target" | ".git" | "tests" | "benches" | "examples") {
                continue;
            }
            scan_dir(&path, out)?;
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            scan_file(&path, out)?;
        }
    }
    Ok(())
}

fn scan_file(path: &Path, out: &mut Vec<Finding>) -> std::io::Result<()> {
    let src = fs::read_to_string(path)?;
    let rel = normalize(path);
    for (idx, line) in src.lines().enumerate() {
        if line.contains("tokio::spawn(")
            || line.contains("tokio::spawn_blocking(")
            || line.contains("tokio::task::spawn(")
            || line.contains("tokio::task::spawn_blocking(")
        {
            out.push(Finding {
                path: rel.clone(),
                line: idx + 1,
                text: line.to_string(),
            });
        }
    }
    Ok(())
}

fn load_baseline(path: &str) -> std::io::Result<HashSet<String>> {
    let mut out = HashSet::new();
    for line in fs::read_to_string(path)?.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.insert(line.to_string());
    }
    Ok(out)
}

fn normalize(path: &Path) -> String {
    let p = PathBuf::from(path);
    p.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
