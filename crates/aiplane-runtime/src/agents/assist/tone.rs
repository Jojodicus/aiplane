// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 croit GmbH

//! The setup's task-and-tone step on the server: the tone chips and answer
//! languages `TONE_LINES` / `responseText` in `web/src/lib/agent-setup.ts`
//! turn into `main.instructions.response`. The assistant proposes chips by
//! these ids, so applying a proposal selects them instead of leaving a
//! sentence the step cannot read back. Keep the two in step; the test below
//! reads the web file.

/// Each tone chip and the sentence it stands for.
pub const TONES: &[(&str, &str)] = &[
    ("friendly", "Be warm and friendly."),
    ("factual", "Stay factual and neutral."),
    ("casual", "Keep the tone casual and relaxed."),
    ("brief", "Keep answers short and to the point."),
    ("detailed", "Give thorough, detailed answers."),
    ("formal", "Address the visitor formally."),
    ("informal", "Address the visitor informally."),
];

/// The visitor's own language.
pub const VISITOR_LANGUAGE: &str = "visitor";
/// Nothing said about the language.
pub const NO_LANGUAGE: &str = "none";
const VISITOR_LANGUAGE_LINE: &str = "Answer in the language the visitor writes in.";
/// The languages an agent can be told to always answer in, by code.
pub const ANSWER_LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("de", "German"),
    ("fr", "French"),
    ("es", "Spanish"),
    ("ru", "Russian"),
    ("zh", "Chinese"),
];

pub fn tone_ids() -> Vec<String> {
    TONES.iter().map(|(id, _)| id.to_string()).collect()
}

/// Every value a proposal's `language` may take.
pub fn language_choices() -> Vec<String> {
    [VISITOR_LANGUAGE, NO_LANGUAGE]
        .into_iter()
        .chain(ANSWER_LANGUAGES.iter().map(|(code, _)| *code))
        .map(str::to_string)
        .collect()
}

/// `main.instructions.response` for these chips, language and further
/// text, line for line as the setup writes it.
pub fn response_text(chips: &[String], language: Option<&str>, extra: &str) -> String {
    let mut lines: Vec<String> = TONES
        .iter()
        .filter(|(id, _)| chips.iter().any(|c| c == id))
        .map(|(_, line)| line.to_string())
        .collect();
    match language {
        Some(VISITOR_LANGUAGE) => lines.push(VISITOR_LANGUAGE_LINE.into()),
        Some(code) => {
            if let Some((_, name)) = ANSWER_LANGUAGES.iter().find(|(c, _)| *c == code) {
                lines.push(format!("Always answer in {name}."));
            }
        }
        None => {}
    }
    if !extra.trim().is_empty() {
        lines.push(extra.trim().to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chips_and_language_become_the_setups_lines_in_its_order() {
        let text = response_text(
            &["brief".into(), "friendly".into()],
            Some("de"),
            "Use the visitor's name.",
        );
        assert_eq!(
            text,
            "Be warm and friendly.\nKeep answers short and to the point.\nAlways answer in \
             German.\nUse the visitor's name."
        );
        assert_eq!(
            response_text(&[], Some(VISITOR_LANGUAGE), ""),
            VISITOR_LANGUAGE_LINE
        );
        assert_eq!(response_text(&[], Some("xx"), " "), "");
    }

    #[test]
    fn the_lines_are_the_ones_the_web_setup_reads_back() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web/src/lib/agent-setup.ts");
        let web = std::fs::read_to_string(&path).expect("the web setup module");
        for (id, line) in TONES {
            assert!(
                web.contains(&format!("{id}: '{line}'")),
                "`{id}: '{line}'` is not in {} — keep TONE_LINES and this list in step",
                path.display()
            );
        }
        for (code, name) in ANSWER_LANGUAGES {
            assert!(web.contains(&format!("{code}: '{name}'")), "{code}");
        }
        assert!(web.contains(VISITOR_LANGUAGE_LINE));
        assert!(web.contains("`Always answer in ${LANGUAGE_NAMES[lang]}.`"));
    }
}
