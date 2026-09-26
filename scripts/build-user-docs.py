#!/usr/bin/env python3
"""Build docs/user/ multi-page user documentation from user_guide_final.md."""

from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "docs" / "user_guide_final.md"
OUT = ROOT / "docs" / "user"

PAGES = [
    ("accueil", "Accueil", "accueil.md", None),
    ("installation", "Installation", "installation.md", r"^## 1\. "),
    ("configuration", "Configuration", "configuration.md", r"^## 2\. "),
    ("commandes", "Commandes", "commandes.md", r"^## 3\. "),
    ("interfaces", "Interfaces", "interfaces.md", r"^## 6\. "),
    ("mission", "Mission autonome", "mission.md", None),
    ("donnees", "Données et mémoire", "donnees.md", None),
    ("workspace", "Workspace", "workspace.md", None),
    ("extensions", "Skills, plugins et canaux", "extensions.md", r"^## (9|10)\. "),
    ("depannage", "Dépannage", "depannage.md", r"^## 12\. "),
    ("nouveautes", "Nouveautés", "nouveautes.md", r"^## 14\. "),
]

SECTION_RE = re.compile(r"^## \d+(?:bis)?\.\s+.*", re.MULTILINE)


def split_sections(text: str) -> list[tuple[str, str]]:
    parts = SECTION_RE.split(text)
    headers = SECTION_RE.findall(text)
    if not parts:
        return [("", text)]
    out: list[tuple[str, str]] = []
    if parts[0].strip():
        out.append(("", parts[0]))
    for h, body in zip(headers, parts[1:]):
        out.append((h, body))
    return out


def sections_matching(sections: list[tuple[str, str]], pattern: str) -> str:
    rx = re.compile(pattern)
    chunks = [f"## {h}\n{b}" if h else b for h, b in sections if h and rx.search(h)]
    return "\n\n---\n\n".join(chunks).strip()


def clean_user_text(text: str) -> str:
    replacements = [
        (r"\*\*Maintenance\*\* :.*?\n\n", ""),
        (r"Des \*\*captures d.?écran\*\*.*?\n\n", ""),
        (r"Le site public \*\*Akasha_app\*\*.*?\n\n", ""),
        (r"une checklist de synchronisation figure dans le dépôt \*\*Akasha_app\*\*.*?\n", ""),
        (r"`docs/DOCUMENTATION_SYNC\.md`", ""),
        (r"`apps/akasha-ui`[^.\n]*", "l'interface desktop"),
        (r"Playwright du dépôt source[^.\n]*", "des tests automatisés"),
        (r"cas habituel du dépôt source lancé depuis la racine du projet", "installation depuis les sources"),
        (r"`spec/35_configuration_reference\.md` \(référence technique complète\)", "cette documentation"),
        (r"`spec/llm_router\.example\.yaml`", "`llm_router.yaml`"),
        (r"`spec/tools_policy\.example\.yaml`", "`tools_policy.yaml`"),
        (r"`spec/autonomous_mission\.example\.yaml`", "`autonomous_mission.yaml`"),
        (r"voir le guide complet", "voir les autres pages de cette documentation"),
    ]
    for old, new in replacements:
        text = re.sub(old, new, text, flags=re.IGNORECASE | re.DOTALL)
    return text.strip() + "\n"


ACCUEIL_BODY = """# Guide utilisateur Akasha

Bienvenue dans la documentation intégrée d'Akasha. Utilisez le menu à gauche (interface desktop) ou les pages ci-dessous pour naviguer.

## Sommaire

| Page | Contenu |
|------|---------|
| Installation | Téléchargement, scripts setup, premier lancement |
| Configuration | Fichiers du data_dir, variables d'environnement, exemples YAML |
| Commandes | CLI `akasha` et commandes slash dans le chat |
| Interfaces | TUI et application desktop : onglets, raccourcis |
| Mission autonome | Heartbeats, objectifs de fond, rapports |
| Données et mémoire | RAG utilisateur, mémoire, graphe projet |
| Workspace | Cookbook, Comparer, Recherche, Notes (v0.9) |
| Skills, plugins et canaux | Extensions, galerie, Telegram, Matrix, etc. |
| Dépannage | `akasha doctor`, problèmes fréquents |
| Nouveautés | Changements récents |

## Parcours rapide

1. `akasha init` puis `akasha start`
2. `akasha doctor` pour vérifier l'installation
3. Chat dans l'application desktop ou `akasha tui`
4. Consultez **Workspace** pour le Cookbook et la comparaison de modèles

Le daemon écoute par défaut sur le port **3876** (`AKASHA_PORT`).
"""

WORKSPACE_BODY = """# Workspace (interface desktop)

Depuis la version **0.9.0**, l'application desktop regroupe plusieurs outils dans le groupe **Workspace** de la barre latérale.

## Navigation par groupes

| Groupe | Onglets |
|--------|---------|
| Principal | Chat (1) |
| Workspace | Comparer (2), Recherche (3), Cookbook, Notes |
| Opérations | Tâches (4), Retours planifiés, Routeur (5), Mission (9) |
| Données | Calendrier (6), Mémoire (7) |
| Aide | Documentation (8), Paramètres |

Les touches **1 à 9** basculent vers l'onglet correspondant (inactives si le focus est dans un champ de saisie).

## Cookbook

Deux vues :

- **Modèles** : recommandations matérielles, catalogue Hugging Face, modèles Ollama locaux, ajout au routeur LLM, pull de modèles.
- **Recettes** : guides pas-à-pas (prompts, agents, RAG, fine-tuning, évaluation, ML, déploiement). Actions : essayer dans le chat, ouvrir Comparer, installer un skill lié.

## Comparer

Comparez des réponses de plusieurs modèles sur la même consigne. Mode **aveugle** disponible pour évaluer sans biais de marque. Indications de coût lorsque configuré.

## Recherche approfondie

Workflow de recherche web structurée : découverte, lecture, validation, synthèse avec sources.

## Notes

Éditeur de notes (TipTap) synchronisé avec le daemon. Utile pour brouillons, comptes-rendus et contexte projet.
"""

MISSION_BODY = """# Mission autonome

La **mission autonome** permet à Akasha de poursuivre un objectif de fond via des **heartbeats** périodiques.

## Interface

Onglet **Mission** (application desktop) ou fichier **`autonomous_mission.yaml`** dans le data_dir.

## Champs principaux (`autonomous_mission.yaml`)

| Champ | Description |
|-------|-------------|
| `enabled` | Active la mission |
| `objective` | Objectif en langage naturel |
| `global_context` | Contexte injecté à chaque cycle |
| `horizon` | `short`, `medium`, `long` |
| `heartbeat_interval_minutes` | Fréquence des cycles |
| `report_dir` | Dossier des rapports (relatif au data_dir) |
| `session_id` | Session chat liée (mode sans questions) |
| `status` | `active`, `paused`, etc. |

## API (daemon local)

- `GET` / `PUT /api/autonomous-mission` — lire / mettre à jour
- `POST /api/autonomous-mission/pause` | `/resume`
- `GET /api/autonomous-mission/events` — journal (`limit`, `since`)

Les rapports Markdown sont écrits sous le répertoire configuré après chaque heartbeat réussi.

## Life layer (packs planifiés)

En complément de la mission autonome, l’onglet **Calendrier → Récurrences** propose un panneau **Life layer** :

| Pack | Rôle |
|------|------|
| **Brief matinal** | Schedule quotidien ; le résultat est poussé sur Telegram (`AKASHA_TELEGRAM_NOTIFY_CHAT_ID`) via `POST /api/channels/notify` |
| **Pack nuit** | Passage nocturne avec rapport (skills / agenda / follow-ups) |
| **Langage naturel** | Phrase du type « chaque matin à 7h30, brief Telegram » → aperçu puis création (`POST /api/schedules/from-nl`, CLI `akasha schedule from-nl`) |

Prérequis brief canal : connecteur Telegram activé + chat id notify (Paramètres → Connecteurs).

Détail cycle / contrats / observabilité (contributeurs) : `spec/dev/runtime/autonomous-mission.md`.
"""

DONNEES_BODY = """# Données, RAG et mémoire

## RAG utilisateur

Paramètres → **Données** → **RAG utilisateur** : indexez des documents texte ; l'agent reçoit des extraits pertinents dans le contexte.

API : `/api/user-rag/...`

## Mémoire

Onglet **Mémoire** : mémoire court terme (session) et long terme (persistante). Recherche, vue graphe, suppression d'entrées.

## Graphe projet

Paramètres → **Données** → **Graphe projet** : enregistrez plusieurs racines de projet ; index SQLite ; rapports HTML sous `workspace_graph/out/<id>/` dans le data_dir.

Outil agent : `workspace_graph_search` (si autorisé dans `tools_policy.yaml`).

API : `/api/workspace-graph/workspaces`
"""


def build_mission(sections: list[tuple[str, str]]) -> str:
    chunks = []
    for h, b in sections:
        if "autonomous_mission" in h.lower() or "mission" in h.lower():
            chunks.append(f"## {h}\n{b}" if h else b)
    base = MISSION_BODY
    extra = "\n\n".join(chunks).strip()
    return clean_user_text(base + ("\n\n" + extra if extra else ""))


def build_extensions(sections: list[tuple[str, str]]) -> str:
    base = sections_matching(sections, r"^## (9|10)\. ")
    plugins = """
## Plugins WASM

Installez depuis un dossier cloné : `akasha plugin install CHEMIN`.

Catalogue public : page **Plugins** sur https://azerothl.github.io/Akasha_app/plugins.html

Plugins **sidecar** (canaux Matrix, CalDAV, **Home Assistant événements**) : processus compagnon ; voir le README de chaque plugin.

### Réseau host (sandbox HTTP)

Les plugins WASM n’ont pas d’accès réseau libre. Pour autoriser des appels HTTP :

1. Dans le `manifest.toml` du plugin : `permissions = ["network"]`.
2. Section `[network]` avec `allowed_url_prefixes`, `https_only`, `max_response_bytes`, `timeout_ms`, `max_requests_per_run`.

L’host expose uniquement `akasha::http_fetch` (JSON in/out). Sans permission `network` ou sans préfixe autorisé, les appels sont refusés. Détail : `spec/dev/plugins/plugin-host-network.md`.

### Vue carte (`view: "map"`)

Un outil (plugin ou natif) peut renvoyer un JSON avec `"view": "map"` pour afficher une carte dans l’UI (géométrie GeoJSON-like `[lon, lat]`, routes, étapes, bbox, liens OSM optionnels). Contrat : `spec/dev/plugins/plugin-map-view-schema.md`.

### Home Assistant (domotique)

Akasha pilote **Home Assistant** (Zigbee, Z-Wave, Matter via HA) — pas un hub radio natif.

**Prérequis HA**

1. Installer [Home Assistant](https://www.home-assistant.io/installation/).
2. Créer un jeton d'accès long-lived (Profil → Sécurité).
3. Détecter l'URL : `akasha discover homeassistant` ou Réglages → Connecteurs → **Détecter**.

**Configuration Akasha**

- `connectors.env` : `AKASHA_HOMEASSISTANT_ENABLED=1`, `HA_BASE_URL=http://…`
- Vault : `akasha vault set ha_access_token VOTRE_TOKEN`
- Plugin : `akasha plugin install CHEMIN/vers/Akasha_plugins/plugins/homeassistant`
- Sidecar (événements → webhooks) : voir `plugins/homeassistant/README.md`

**Outils agent** : `ha_get_state`, `ha_list_entities`, `ha_call_service`, `ha_run_script` (plugin `homeassistant`).

## Code Studio

Interface opérateur pour projets code (cockpit, éditeur, build, preview) :

```bash
npx akasha-code-studio@0.9.0
```

Nécessite le daemon Akasha sur le port 3876.
"""
    return clean_user_text("# Skills, plugins et canaux\n\n" + base + "\n\n" + plugins)


def main() -> None:
    text = SOURCE.read_text(encoding="utf-8")
    sections = split_sections(text)
    OUT.mkdir(parents=True, exist_ok=True)

    writers = {
        "accueil.md": lambda: ACCUEIL_BODY,
        "installation.md": lambda: "# Installation\n\n" + sections_matching(sections, r"^## 1\. "),
        "configuration.md": lambda: "# Configuration\n\n"
        + sections_matching(sections, r"^## 2\. ")
        + "\n\n"
        + sections_matching(sections, r"^## 2bis\. ")
        + "\n\n"
        + sections_matching(sections, r"^## 5\. ")
        + "\n\n"
        + sections_matching(sections, r"^## 8\. ")
        + "\n\n"
        + sections_matching(sections, r"^## 8bis\. "),
        "commandes.md": lambda: "# Commandes\n\n"
        + sections_matching(sections, r"^## 3\. ")
        + "\n\n"
        + sections_matching(sections, r"^## 7\. ")
        + "\n\n"
        + sections_matching(sections, r"^## 7bis\. ")
        + "\n\n"
        + sections_matching(sections, r"^## 13\. "),
        "interfaces.md": lambda: build_interfaces(sections),
        "mission.md": lambda: build_mission(sections),
        "donnees.md": lambda: DONNEES_BODY,
        "workspace.md": lambda: WORKSPACE_BODY,
        "extensions.md": lambda: build_extensions(sections),
        "depannage.md": lambda: "# Dépannage\n\n"
        + sections_matching(sections, r"^## 12\. ")
        + "\n\n"
        + sections_matching(sections, r"^## 11\. "),
        "nouveautes.md": lambda: build_nouveautes(sections),
    }

    for _id, _title, filename, _ in PAGES:
        body = clean_user_text(writers[filename]())
        (OUT / filename).write_text(body, encoding="utf-8")
        print(f"Wrote {OUT / filename}")

    index = {
        "default": "accueil",
        "pages": [{"id": pid, "title": title, "file": fname} for pid, title, fname, _ in PAGES],
    }
    (OUT / "index.json").write_text(json.dumps(index, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"Wrote {OUT / 'index.json'}")


def build_interfaces(sections: list[tuple[str, str]]) -> str:
    body = sections_matching(sections, r"^## 6\. ")
    body = body.replace("Mission = **8**, Paramètres = **9**", "Documentation = **8**, Mission = **9**")
    body = re.sub(
        r"- \*\*Onglets\*\* : Chat, \*\*Retours planifiés\*\*, Routeur, Documentation, Tâches, Calendrier, Mémoire, \*\*Mission\*\*, Paramètres\.",
        "- **Onglets** : Chat (1), Comparer (2), Recherche (3), Cookbook, Notes, Tâches (4), Retours planifiés, Routeur (5), Calendrier (6), Mémoire (7), Documentation (8), Mission (9), Paramètres.",
        body,
    )
    return clean_user_text("# Interfaces\n\n" + body + "\n\nVoir aussi la page **Workspace** pour Cookbook, Comparer et Recherche.")


def build_nouveautes(sections: list[tuple[str, str]]) -> str:
    old = sections_matching(sections, r"^## 14\. ")
    v010 = """
## Nouveautés 0.10.0

- **First-use embarqué** : wizard UI (statut, download GGUF, multi-modèle, premier message test)
- **Artefacts** : zip CPU (Candle) + CUDA / full CUDA (llama-cpp-4 + GGUF)
- **Cockpit** : Active work, modes composer, Usage 7/30 j, pin/fork/dual-pane
- **Life layer** : overnight, brief matinal Telegram, OAuth Connectors, schedules en langage naturel
- **Companion LAN** : APIs daemon pour client ESP32 (pairing / discovery)
"""
    v09 = """
## Nouveautés 0.9.0

- **Workspace** : Cookbook (Modèles + Recettes), Comparer, Recherche approfondie, Notes
- **Navigation** : barre latérale par groupes (Principal, Workspace, Opérations, Données, Aide)
- **Documentation** : pages thématiques dans l'onglet Doc
- **Plugins canaux** : Matrix et CalDAV (sidecar) dans le catalogue public
- **Code Studio** : `npx akasha-code-studio@0.9.0`
"""
    return clean_user_text("# Nouveautés\n\n" + v010 + "\n\n---\n\n" + v09 + "\n\n---\n\n" + old)


if __name__ == "__main__":
    main()
