//! Supplemental physical checks for unambiguous, explicitly named file creations.
//! Presence is never proof of content, build success, or complete acceptance.
use serde_json::{json, Value};
use std::path::Path;

pub(super) fn can_retry_empty(goals:&[String], successful:u32, failed:u32, checks:&[Value]) -> bool {
    !goals.is_empty() && successful==0 && failed==0
        && !checks.iter().any(|check|check["kind"]=="empty_generation_retry")
}

/// Only explicit single-line literals, never a semantic interpretation of prose.
/// Unsupported wording remains unchecked. No content is generated or rewritten here.
pub(super) fn exact_file_literals(message:&str) -> Vec<(String,String)> {
    let request=super::studio_conversation::user_request(message);
    let goals=file_goals(message);
    let literal=r#"(?:`([^`\r\n]*)`|"([^"\r\n]*)"|([\p{L}\p{N}_:/+@=-]{1,128}))(?:[.!?](?:\s|$)|\s*$)"#;
    let mut result=vec![];
    for path in &goals {
        let pattern=format!(r#"(?i)(?:`{0}`|"{0}"|\b{0})\s+(?:avec exactement|with exactly|containing exactly)\s+{literal}"#,regex::escape(path));
        let grammar=regex::Regex::new(&pattern).unwrap();
        let matches:Vec<_>=grammar.captures_iter(request).collect();
        if let [captures]=matches.as_slice() {
            let content=(1..=3).find_map(|index|captures.get(index)).unwrap().as_str();
            result.push((path.clone(),content.to_owned()));
        }
    }
    if goals.len()==1 && result.is_empty() {
        let pattern=format!(r#"(?i)(?:Son contenu doit être exactement la ligne|Its content must be exactly the line)\s+{literal}"#);
        if let Some(captures)=regex::Regex::new(&pattern).unwrap().captures(request) {
            let content=(1..=3).find_map(|index|captures.get(index)).unwrap().as_str();
            result.push((goals[0].clone(),content.to_owned()));
        }
    }
    result
}

pub(super) fn literal_write_error(root:&Path, literals:&[(String,String)], tool:&str, args:&[String]) -> Option<String> {
    if !matches!(tool,"write_file"|"write_code") {return None;}
    let encoded=args.first().and_then(|raw|serde_json::from_str::<Value>(raw).ok());
    let (raw_path,raw_content)=if let Some(encoded)=encoded.as_ref().filter(|value|value.is_object()) {
        (encoded["path"].as_str()?,encoded["content"].as_str()?.to_owned())
    }else{(args.first()?.as_str(),args.get(1..)?.join(" "))};
    let path=crate::studio_tool_contract::bounded_path(root,raw_path).ok()?;
    let (_,expected)=literals.iter().find(|(goal,_)|crate::studio_tool_contract::bounded_path(root,goal).ok().as_ref()==Some(&path))?;
    let content=raw_content.replace("\r\n","\n");
    if content==*expected || content==format!("{expected}\n") {return None;}
    Some(json!({"schema_version":1,"success":false,"error_code":"explicit_content_mismatch","error":"No file written: the proposed content differs from the user's explicit literal. Request the tool again with these exact bytes; do not change case or add a template.","path":raw_path,"expected_literal":expected}).to_string())
}

pub(super) fn requires_tool_retry(prompt: &str, checks: &[Value]) -> bool {
    prompt.starts_with("[Pending physical deliverables]")
        && checks.iter().any(|check|check["kind"]=="requested_file" && check["status"]=="failed")
}

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
    fn empty_generation_retry_is_once_before_effects_and_is_durable_in_checks() {
        let goals=vec!["output.txt".into()];
        assert!(can_retry_empty(&goals,0,0,&[]));
        for (goals,success,failed,checks) in [(vec![],0,0,vec![]),(goals.clone(),1,0,vec![]),(goals.clone(),0,1,vec![]),(goals,0,0,vec![json!({"kind":"empty_generation_retry","status":"retried"})])] {
            assert!(!can_retry_empty(&goals,success,failed,&checks));
        }
    }
    #[test]
    fn literal_contract_is_narrow_case_preserving_and_never_inferred_from_project_context() {
        assert_eq!(exact_file_literals("Crée literal-case.txt avec exactement token-42. CASE:literal-content"),vec![("literal-case.txt".into(),"token-42".into())]);
        assert_eq!(exact_file_literals("Crée résultat.txt avec exactement éTé-42. Ensuite crée `second.txt` avec exactement \"hello world!\"."),vec![("résultat.txt".into(),"éTé-42".into()),("second.txt".into(),"hello world!".into())]);
        assert_eq!(exact_file_literals("Create config.txt with exactly `Token=Value`."),vec![("config.txt".into(),"Token=Value".into())]);
        assert!(exact_file_literals("Crée article.md avec exactement 3 paragraphes.").is_empty());
        assert!(exact_file_literals("Crée a.txt avec exactement one. Ensuite crée a.txt avec exactement two.").is_empty());
        assert!(exact_file_literals("Explique comment créer result.txt avec exactement BAD.").is_empty());
        let wrapped="Crée bad.txt avec exactement BAD.\n[Current user request — task scope]\nCrée bon.txt avec exactement Bon.\n[End current user request]";
        assert_eq!(exact_file_literals(wrapped),vec![("bon.txt".into(),"Bon".into())]);
        let dir=tempfile::tempdir().unwrap();let literals=vec![("result.txt".into(),"éTé".into())];
        assert!(literal_write_error(dir.path(),&literals,"write_file",&["workspace:/result.txt".into(),"ÉTÉ".into()]).is_some());
        let decoded=crate::studio_tool_contract::decode(&json!({"schema_version":1,"tool":"write_file","arguments":{"path":"workspace:/result.txt","content":"ÉTÉ","expected_version":"missing"}}).to_string()).unwrap();
        assert!(literal_write_error(dir.path(),&literals,&decoded.envelope.tool,&decoded.argv).is_some());
        assert!(literal_write_error(dir.path(),&literals,"write_file",&["result.txt".into(),"éTé\r\n".into()]).is_none());
        assert!(literal_write_error(dir.path(),&literals,"write_file",&["other.txt".into(),"ÉTÉ".into()]).is_none());
        assert!(!dir.path().join("result.txt").exists());
    }
    #[test]
    fn failed_physical_check_requires_an_action_only_for_its_next_repair_turn() {
        let failed=vec![json!({"kind":"requested_file","status":"failed"})];
        assert!(requires_tool_retry("[Pending physical deliverables] missing output",&failed));
        assert!(!requires_tool_retry("",&failed));
        assert!(!requires_tool_retry("[Pending physical deliverables] missing output",&[]));
        assert!(!requires_tool_retry("[Pending physical deliverables] missing output",&[json!({"kind":"requested_file","status":"passed"})]));
    }
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
