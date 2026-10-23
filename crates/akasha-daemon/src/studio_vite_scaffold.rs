//! Versioned infrastructure only. Product features remain the agents' work.
use std::{fs, io, path::Path};

const FILES: &[(&str, &str)] = &[
    ("package.json", include_str!("studio_templates/vite-react-ts/package.json")),
    ("package-lock.json", include_str!("studio_templates/vite-react-ts/package-lock.json")),
    ("tsconfig.json", include_str!("studio_templates/vite-react-ts/tsconfig.json")),
    ("index.html", include_str!("studio_templates/vite-react-ts/index.html")),
    ("src/main.tsx", include_str!("studio_templates/vite-react-ts/main.tsx")),
    ("src/App.tsx", include_str!("studio_templates/vite-react-ts/App.tsx")),
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
    fs::write(root.join("STUDIO_SCAFFOLD.md"), "# Socle Vite / React / TypeScript — v1\n\nInfrastructure initiale fournie par Code Studio : manifeste, TypeScript strict, entrée HTML et montage React. Aucune fonctionnalité métier n'est préimplémentée. Les agents doivent conserver ces entrées, installer les dépendances, implémenter la demande puis vérifier `npm run build` et les tests pertinents. `npm test` exécute Vitest ; ajouter les tests du produit avant de le lancer.\n")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opt_in_scaffold_is_complete_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("studio-vite-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        assert!(!initialize(&dir, Some("Vite TypeScript")).unwrap());
        assert!(initialize(&dir, Some("Vite 5+, React 18+, TypeScript strict")).unwrap());
        for (file, _) in FILES { assert!(dir.join(file).is_file()); }
        let manifest: serde_json::Value = serde_json::from_str(&fs::read_to_string(dir.join("package.json")).unwrap()).unwrap();
        assert_eq!(manifest["scripts"]["build"], "tsc --noEmit && vite build");
        assert!(initialize(&dir, Some("Vite React TypeScript")).is_err());
        assert!(crate::studio_tool_contract::bounded_path(&dir,"workspace:/ package.json").is_err());
        assert!(crate::studio_tool_contract::bounded_path(&dir,"workspace:/ tsconfig.json").is_err());
        assert!(!dir.join(" package.json").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
