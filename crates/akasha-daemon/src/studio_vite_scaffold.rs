//! Versioned infrastructure only. Product features remain the agents' work.
use std::{fs, io, path::Path};

const FILES: &[(&str, &str)] = &[
    ("package.json", include_str!("studio_templates/vite-react-ts/package.json")),
    ("package-lock.json", include_str!("studio_templates/vite-react-ts/package-lock.json")),
    ("tsconfig.json", include_str!("studio_templates/vite-react-ts/tsconfig.json")),
    ("index.html", include_str!("studio_templates/vite-react-ts/index.html")),
    ("src/main.tsx", include_str!("studio_templates/vite-react-ts/main.tsx")),
    ("src/App.tsx", include_str!("studio_templates/vite-react-ts/App.tsx")),
    ("vitest.config.ts", include_str!("studio_templates/vite-react-ts/vitest.config.ts")),
    ("src/test/setup.ts", include_str!("studio_templates/vite-react-ts/setup.ts")),
    (".gitignore", "node_modules/\ndist/\n*.local\n"),
];

pub(crate) fn initialize(root: &Path, stack: Option<&str>) -> io::Result<bool> {
    let stack = stack.unwrap_or_default().to_ascii_lowercase();
    if !["vite", "react", "typescript"].iter().all(|part| stack.contains(part)) {
        return Ok(false);
    }
    // Called only on creation, before the initial Git commit. Never overwrite a
    // project or partially migrate an imported tree.
    if FILES.iter().any(|(path, _)| root.join(path).exists()) {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "Vite scaffold destination is not empty"));
    }
    for (relative, content) in FILES {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("template parent"))?;
        fs::write(path, content)?;
    }
    fs::write(root.join("STUDIO_SCAFFOLD.md"), "# Socle Vite / React / TypeScript — v2\n\nInfrastructure fournie : manifeste verrouillé, TypeScript strict, entrée HTML, montage React et environnement Vitest DOM. Aucune fonctionnalité métier ni test produit n'est préimplémenté.\n\nPhase socle : vérifier `npm run build`, sans réécrire la configuration ni ajouter des fonctionnalités/tests métier à ce ticket. Une vérification réussie permet la revue puis le passage au ticket d'implémentation. Phase fonctionnalités : implémenter la demande, ajouter les tests produit, exécuter `npm test` et `npm run build`.\n\nTests : configuration unique `vitest.config.ts` à la racine, jsdom, globals Vitest et matchers jest-dom déjà chargés par `src/test/setup.ts`. Les tests .test.ts/.test.tsx sous src restent inclus dans TypeScript strict. Ne pas dupliquer la configuration sous src, ajouter de shim de types, exclure les tests du build ou installer @vitest/ui pour résoudre une erreur de test. Utiliser les versions verrouillées existantes.\n")?;
    Ok(true)
}

/// Only the persisted system foundation ticket can use the supplied-infrastructure
/// boundary. A title/prompt or arbitrary specialist request is not authority.
pub(crate) fn foundation_ticket(root: &Path, task: &str, agent: &str) -> Option<String> {
    if agent != "studio_scaffold" || !root.join("STUDIO_SCAFFOLD.md").is_file() { return None; }
    if !FILES.iter().all(|(path,_)|root.join(path).is_file()) {return None;}
    let tickets=crate::api_studio::studio_list_tickets(root);
    tickets.iter().find(|t|
        t.related_task_id.as_deref()==Some(task) && t.requested_by=="system_initial_backlog"
        && t.parent_ticket_id.as_ref().is_some_and(|parent|t.creation_key.as_deref()==Some(format!("initial:{parent}:foundation").as_str()))
        && tickets.iter().any(|parent|Some(&parent.id)==t.parent_ticket_id.as_ref()&&crate::api_studio::is_initial_backlog_root(parent))
        && t.assigned_agent=="studio_scaffold" && t.status=="in_progress")
        .map(|t|t.id.clone())
}

pub(crate) fn foundation_definition(names: Vec<String>, ticket: &str, args: &[String], ready: bool) -> serde_json::Value {
    use serde_json::{json,Value};
    let mut definition=crate::studio_tool_contract::native_definition(names);
    if let Some(variants)=definition.pointer_mut("/function/parameters/oneOf").and_then(Value::as_array_mut) {
        for variant in variants {
            let schema=&mut variant["properties"]["arguments"];
            if ready {
                schema["properties"]["ticket_id"]["enum"]=json!([ticket]);
                schema["properties"]["status"]["enum"]=json!(["review"]);
                schema["required"]=json!(["ticket_id","status"]);
            } else if let Some(key)=crate::studio_progress::exact_command_key("run_command",args) {
                let parts:Value=serde_json::from_str(&key).expect("validated command key");
                for (field,value) in [("program",parts[0].clone()),("cwd",parts[1].clone()),("args",parts[2].clone())] {schema["properties"][field]["enum"]=json!([value]);}
                schema["required"]=json!(["program","cwd","args"]);
            }
        }
    }
    definition
}

pub(crate) fn foundation_action_allowed(ticket:&str,command:&[String],ready:bool,tool:&str,args:&[String])->bool {
    if !ready {return tool=="run_command" && crate::studio_progress::exact_command_key(tool,args)==crate::studio_progress::exact_command_key("run_command",command);}
    tool=="studio_update_ticket" && args.first().and_then(|s|serde_json::from_str::<serde_json::Value>(s).ok()).is_some_and(|v|
        v["ticket_id"]==ticket && v["status"]=="review" && v.as_object().is_some_and(|o|o.keys().all(|k|matches!(k.as_str(),"ticket_id"|"status"|"evidence"))))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foundation_boundary_binds_the_command_and_review_without_editing_acceptance() {
        let command=vec!["--cwd".into(),"workspace:/".into(),"npm".into(),"run".into(),"build".into()];
        assert!(foundation_action_allowed("ticket",&command,false,"run_command",&command));
        assert!(!foundation_action_allowed("ticket",&command,false,"write_file",&["workspace:/src/App.tsx".into()]));
        for payload in [r#"{"ticket_id":"other","status":"review"}"#,r#"{"ticket_id":"ticket","status":"done"}"#,r#"{"ticket_id":"ticket","status":"review","acceptance_criteria":[]}"#] {
            assert!(!foundation_action_allowed("ticket",&command,true,"studio_update_ticket",&[payload.into()]));
        }
        assert!(foundation_action_allowed("ticket",&command,true,"studio_update_ticket",&[r#"{"ticket_id":"ticket","status":"review"}"#.into()]));
    }
    #[test]
    fn opt_in_scaffold_is_complete_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("studio-vite-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        assert!(!initialize(&dir, Some("Vite TypeScript")).unwrap());
        assert!(initialize(&dir, Some("Vite 5+, React 18+, TypeScript strict")).unwrap());
        for (file, _) in FILES { assert!(dir.join(file).is_file()); }
        let manifest: serde_json::Value = serde_json::from_str(&fs::read_to_string(dir.join("package.json")).unwrap()).unwrap();
        assert_eq!(manifest["scripts"]["build"], "tsc --noEmit && vite build");
        assert!(fs::read_to_string(dir.join("vitest.config.ts")).unwrap().contains("environment: 'jsdom'"));
        assert!(fs::read_to_string(dir.join("src/test/setup.ts")).unwrap().contains("jest-dom/vitest"));
        assert!(foundation_ticket(&dir,"unbound","studio_scaffold").is_none());
        assert!(initialize(&dir, Some("Vite React TypeScript")).is_err());
        assert!(crate::studio_tool_contract::bounded_path(&dir,"workspace:/ package.json").is_err());
        assert!(crate::studio_tool_contract::bounded_path(&dir,"workspace:/ tsconfig.json").is_err());
        assert!(!dir.join(" package.json").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
