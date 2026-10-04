// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The docs under `docs/` describe the system as it is, not how it got there
//! (AGENTS.md → "Docs describe the current system, not its history"). History
//! lives in git and the issues; a doc that narrates it ("What #N built", "since
//! #96", "no longer …") grows into a changelog nobody can read and every branch
//! conflicts on.
//!
//! Like the architecture tests, this reads the repository's own files and
//! fails on the shapes that give history away, naming file, line and rule:
//!
//! - a heading that names an issue (`### What #84 built`);
//! - an issue reference (`#120`) anywhere in the prose;
//! - the phrases "no longer" and "instead of", which describe a change rather
//!   than a state ("X rather than Y" states a design choice; "the rows are
//!   gone" or "nothing writes it" state a fact).
//!
//! Fenced code blocks and inline code spans are skipped: `#242427` is a
//! colour, and quoted code may say anything. A file that legitimately records
//! history goes on `HISTORY_DOCS` with the reason; an entry no hit needs any
//! more fails too, so the list cannot rot into a blanket exemption.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// A doc allowed to reference issues and history, and why.
struct Allowed {
    file: &'static str,
    why: &'static str,
}

/// Empty on purpose: every doc in `docs/` describes the current system. An
/// entry here is a conscious exception — a changelog-type file whose subject
/// is history — not a way to keep a "What #N built" section.
const HISTORY_DOCS: &[Allowed] = &[];

const PHRASES: &[&str] = &["no longer", "instead of"];

fn docs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs")
        .canonicalize()
        .expect("docs/ sits two levels above the gateway crate")
}

/// `text` with fenced code blocks and inline code spans blanked, newlines
/// kept, so a line number in the result is one in the file.
fn prose(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<String> = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let opener: String = trimmed
            .chars()
            .take_while(|&c| c == '`' || c == '~')
            .collect();
        let is_fence = opener.len() >= 3 && opener.chars().all(|c| opener.starts_with(c));
        match &fence {
            Some(open) if is_fence && opener.starts_with(open.as_str()) => {
                fence = None;
                out.push_str(&blank(line));
            }
            Some(_) => out.push_str(&blank(line)),
            None if is_fence => {
                fence = Some(opener);
                out.push_str(&blank(line));
            }
            None => out.push_str(line),
        }
    }
    without_code_spans(&out)
}

fn blank(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' { '\n' } else { ' ' })
        .collect()
}

/// Inline code spans blanked: a run of N backticks up to the next run of
/// exactly N, across line breaks like Markdown's own spans. An unclosed run
/// is literal text.
fn without_code_spans(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = chars.clone();
    let run_at = |i: usize| chars[i..].iter().take_while(|&&c| c == '`').count();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        let n = run_at(i);
        let mut j = i + n;
        let mut close = None;
        while j < chars.len() {
            if chars[j] == '`' {
                let m = run_at(j);
                if m == n {
                    close = Some(j);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        match close {
            Some(j) => {
                for c in &mut out[i..j + n] {
                    if *c != '\n' {
                        *c = ' ';
                    }
                }
                i = j + n;
            }
            None => i += n,
        }
    }
    out.into_iter().collect()
}

/// The issue references in `line`: `#` and digits, not inside a word
/// (`page#12`, `&#39;`) and not the start of an anchor (`#5-visitors`).
fn issue_refs(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    for (i, _) in line.match_indices('#') {
        if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'&') {
            continue;
        }
        let digits = bytes[i + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits == 0 {
            continue;
        }
        let after = bytes.get(i + 1 + digits).copied();
        if after.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            continue;
        }
        found.push(line[i..i + 1 + digits].to_string());
    }
    found
}

fn is_heading(line: &str) -> bool {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|&c| c == '#').count();
    (1..=6).contains(&hashes) && t[hashes..].starts_with(' ')
}

/// `(rule, matched)` for every history shape on one prose line.
fn history_on(line: &str) -> Vec<(&'static str, String)> {
    let mut hits = Vec::new();
    let refs = issue_refs(line);
    if is_heading(line) && !refs.is_empty() {
        hits.push((
            "a heading names an issue — title the section by its topic",
            line.trim().to_string(),
        ));
    } else {
        for r in refs {
            hits.push((
                "an issue reference — say what the system does; git and the issues keep who built it when",
                r,
            ));
        }
    }
    let lower = line.to_lowercase();
    for phrase in PHRASES {
        if lower.contains(phrase) {
            hits.push((
                "history phrasing — describe the current state, or state the design choice (\"X rather than Y\")",
                (*phrase).to_string(),
            ));
        }
    }
    hits
}

#[test]
fn docs_describe_the_current_system_not_its_history() {
    let dir = docs_dir();
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("docs/ is readable")
        .map(|e| e.expect("a docs/ entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no Markdown found in {}", dir.display());

    let mut violations = Vec::new();
    let mut used = BTreeSet::new();
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let text = fs::read_to_string(path).expect("a doc reads as UTF-8");
        let exempt = HISTORY_DOCS.iter().find(|a| a.file == name);
        for (n, line) in prose(&text).lines().enumerate() {
            for (rule, matched) in history_on(line) {
                match exempt {
                    Some(a) => {
                        used.insert(a.file);
                    }
                    None => violations.push(format!("docs/{name}:{}: {rule}: {matched}", n + 1)),
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "docs describe the current system, not its history (AGENTS.md → working agreement):\n  {}\n\n\
         Rewrite each as what the system does and why. A file whose subject is history goes on \
         HISTORY_DOCS in crates/aiplane/tests/it/docs_current.rs with the reason.",
        violations.join("\n  ")
    );
    let stale: Vec<String> = HISTORY_DOCS
        .iter()
        .filter(|a| !used.contains(a.file))
        .map(|a| format!("{} ({})", a.file, a.why))
        .collect();
    assert!(
        stale.is_empty(),
        "these HISTORY_DOCS entries match nothing; remove them so the exception cannot be \
         reused silently:\n  {}",
        stale.join("\n  ")
    );
}

#[test]
fn code_is_not_prose() {
    let doc = "Colour `#242427`.\n```text\nWhat #84 built\n```\nA span `across\nlines #9` ends.\n";
    let p = prose(doc);
    assert_eq!(p.lines().count(), doc.lines().count());
    assert!(p.lines().all(|l| history_on(l).is_empty()), "{p}");
}

#[test]
fn issue_references_are_told_from_anchors_and_entities() {
    assert_eq!(issue_refs("since #96, see #87/#88."), ["#96", "#87", "#88"]);
    assert!(issue_refs("[x](#5-visitor-sessions) page#12 &#39; # heading").is_empty());
    assert_eq!(
        history_on("### What #84 built")[0].0,
        "a heading names an issue — title the section by its topic"
    );
    assert_eq!(history_on("It no longer runs.").len(), 1);
}
