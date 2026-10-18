//! Supplemental physical checks for unambiguous, explicitly named file creations.
//! Presence is never proof of content, build success, or complete acceptance.
use serde_json::{json, Value};
use std::path::Path;

pub(super) fn file_goals(message: &str) -> Vec<String> {
    let request = super::studio_conversation::user_request(message);
    if !super::studio_conversation::explicit_file_request(message) {
        return vec![];
    }
    let grammar=regex::Regex::new(r#"(?ix)^\s*(?:(?:ensuite|puis|then|and)\s+)?(?:crée|créer|cree|create|écris|ecris|write)\s+(?:(?:uniquement|seulement|only|le|la|un|une|the|a|new|fichier|file)\s+)*(?:`([^`\r\n]+)`|"([^"\r\n]+)"|((?:workspace:/)?[\p{L}\p{N}_./-]+\.[a-z0-9]{1,12}))"#).expect("literal file goal grammar");
    let mut sentences = vec![];
    let mut start = 0;
    let mut quote = None;
    for (index, ch) in request.char_indices() {
        if matches!(ch, '"' | '`') {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(ch);
            }
        }
        let boundary = ch == '\n'
            || (matches!(ch, '.' | '!' | '?')
                && request[index + ch.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace));
        if quote.is_none() && boundary {
            sentences.push(&request[start..index]);
            start = index + ch.len_utf8();
        }
    }
    sentences.push(&request[start..]);
    let mut paths = vec![];
    // Only a direct creation as the first sentence enables this supplemental check.
    // Quoted examples and explanations must not become new file obligations.
    for (index, sentence) in sentences
        .iter()
        .filter(|s| !s.trim().is_empty())
        .enumerate()
    {
        let Some(captures) = grammar.captures(sentence) else {
            if index == 0 {
                return vec![];
            }
            continue;
        };
        let path = (1..=3)
            .find_map(|group| captures.get(group))
            .unwrap()
            .as_str()
            .to_string();
        if !paths.contains(&path) {
            paths.push(path);
        }
        if paths.len() == 32 {
            break;
        }
    }
    paths
}

pub(super) fn check_files(
    root: &Path,
    policy: &akasha_tools::ToolsPolicy,
    goals: &[String],
) -> Vec<Value> {
    goals.iter().map(|goal| {
        match crate::studio_tool_contract::bounded_path(root,goal) {
            Ok(path) if policy.can_use_tool("read_file") && policy.can_read(&crate::api::strip_verbatim_prefix(path.clone())) => {
                let present=path.is_file();
                json!({"kind":"requested_file","status":if present{"passed"}else{"failed"},"detail":format!("{goal}: {}",if present{"regular file present; content not certified"}else{"requested file missing"}),"path":goal})
            }
            _=>json!({"kind":"requested_file","status":"manual","detail":format!("{goal}: presence cannot be verified under the current workspace/read policy"),"path":goal})
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_outputs_include_later_creation_but_not_reads_or_quoted_examples() {
        assert_eq!(file_goals("Crée FIRST.md avec exactement alpha. Puis lis FIRST.md. Ensuite crée SECOND.md avec exactement beta."),vec!["FIRST.md","SECOND.md"]);
        assert_eq!(file_goals("Crée uniquement le fichier RECIPE_OK.md à la racine du projet. Son contenu doit être exactement la ligne recipe-ok."),vec!["RECIPE_OK.md"]);
        assert_eq!(file_goals("Crée `one.md` avec le texte \"Exemple. Crée unwanted.md\". Ensuite crée \"with space.md\"."),vec!["one.md","with space.md"]);
        assert!(file_goals("Write a description of how to create example.md.").is_empty());
        assert!(file_goals("Explique comment créer example.md.").is_empty());
    }
    #[test]
    fn physical_check_does_not_certify_content_or_escape_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("present.md"),
            "wrong content is not certified",
        )
        .unwrap();
        let policy = akasha_tools::ToolsPolicy {
            allowed_read_paths: vec![dir.path().display().to_string()],
            ..Default::default()
        };
        let checks = check_files(
            dir.path(),
            &policy,
            &[
                "present.md".into(),
                "missing.md".into(),
                "../outside.md".into(),
            ],
        );
        assert_eq!(checks[0]["status"], "passed");
        assert!(checks[0]["detail"]
            .as_str()
            .unwrap()
            .contains("content not certified"));
        assert_eq!(checks[1]["status"], "failed");
        assert_eq!(checks[2]["status"], "manual");
    }
}
