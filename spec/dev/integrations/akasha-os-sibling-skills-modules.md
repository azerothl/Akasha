# Interop sibling — skills / plugins (daemon Akasha) ↔ skills / modules (akasha-os)

> **P9 must-ship (roadmap v0.11)** · Date: 2026-09-26 · Statut: **doc + pilote** (pas de fusion binaire)  
> Repo sibling : [azerothl/akasha-os](https://github.com/azerothl/akasha-os) · Bridge : [sibling-bridge.md](https://github.com/azerothl/akasha-os/blob/main/docs/sibling-bridge.md) · Schémas JSON : [docs/bridge/](https://github.com/azerothl/akasha-os/tree/main/docs/bridge)

## Principe

**Ne pas fusionner** le daemon Akasha (`akasha`) et akasha-os (Preview) en un seul binaire ou installateur. Aligner les **docs**, les **dialectes `SKILL.md`**, et les **chemins d’install** pour éviter de dupliquer les intentions produit. Les ABI Wasmtime (plugins) et `module_rt` / `.aospkg` (modules) restent **séparées** (sibling-bridge : *Partiel — ne pas unifier les ABI encore*).

Doc user courte : [extensions vs akasha-os](../../../docs/user/extensions-vs-akasha-os.md) (renvoi depuis [extensions.md](../../../docs/user/extensions.md)).

---

## Lexique (ne pas confondre)

| Terme | Où | Quoi |
|-------|-----|------|
| **Skill (daemon)** | [Akasha_skills](https://github.com/azerothl/Akasha_skills) · `data_dir/skills/` · `spec/skills/` · [spec 33](../../33_agents_tools_orchestrator_skills.md) | Recette [Agent Skills](https://agentskills.io/specification) : dossier + `SKILL.md` (`name` / `description`) ; galerie `skills.json` ; lock `skills.lock.jsonl` ; API `/api/skills*` |
| **Plugin (daemon)** | [Akasha_plugins](https://github.com/azerothl/Akasha_plugins) · `data_dir/plugins/` | WASM tools-only (`manifest.toml` + `plugin.wasm`) ± sidecar natif ; trust-catalog ; `akasha plugin catalog\|install` |
| **Skill (akasha-os)** | `var/skills/<id>/SKILL.md` · [write-a-skill.md](https://github.com/azerothl/akasha-os/blob/main/docs/write-a-skill.md) · `community/skills/` | Recette Markdown MIT pour agents Preview (`when_to_use`, `tools:`) — **pas** un module WASM |
| **Module (akasha-os)** | `var/modules/` · `.aospkg` · [write-a-module.md](https://github.com/azerothl/akasha-os/blob/main/docs/write-a-module.md) | Extension **dual-surface** (outils + UI déclarative) + **cap review** — **≠** plugin daemon |
| **Sibling bridge** | `aos-bridged` · `127.0.0.1:24710` · [docs/bridge/](https://github.com/azerothl/akasha-os/tree/main/docs/bridge) | HTTP JSON ↔ CBOR bus (`mem.*`, `secrets.*`, UI déclarative) — **pas** un runtime d’extensions partagé |
| **akasha-packages** | [azerothl/akasha-packages](https://github.com/azerothl/akasha-packages) | Satellite naissant — pas de format commun documenté côté daemon |

---

## Mapping conceptuel

| Daemon Akasha | akasha-os | Alignement | Action P9 |
|---------------|-----------|------------|-----------|
| Skill (`SKILL.md`) | Skill (`SKILL.md`) | **Fort** — même idée « recette Markdown » ; dialectes de front matter différents | Matrice + adaptation + pilote partagé |
| Plugin WASM (Wasmtime, tools-only) | Module `.aospkg` (`module_rt`, dual-surface) | **Partiel** — contrats / host_call / UI différents | Doc seule ; **pas** d’unification ABI (A9) |
| Vault / mémoire HTTP daemon | `secrets.*` / `mem.*` via bridge | **Schémas** alignables | Lien schemas ; client HTTP = stretch |
| Canaux chat / Companion / Life layer | Hors noyau OS | Complémentaire | Laisser au sibling daemon |
| Sidecar natif (Matrix, CalDAV, HA) | Cap review + script/rust module | Non équivalent 1:1 | daemon-only / OS-only selon surface |

---

## Matrice de compatibilité `SKILL.md`

Sources : loader daemon [`crates/akasha-daemon/src/skills.rs`](../../../crates/akasha-daemon/src/skills.rs) · [write-a-skill.md](https://github.com/azerothl/akasha-os/blob/main/docs/write-a-skill.md) · agentskills.io.

| Champ front matter | Daemon Akasha | akasha-os Preview | Commun ? | Notes |
|--------------------|---------------|-------------------|----------|-------|
| `name` | **Requis** (idéalement = nom du dossier) | **Requis** (id dossier 2–33 `[a-z][a-z0-9-]*`) | Oui | Même règle pratique |
| `description` | **Requis** (routage / `list_skills`) | **Requis** | Oui | Une ligne « quand utiliser » |
| `license` | Optionnel (ignoré par le loader ; utile galerie) | Attendu (`MIT` pour community) | Portable | Conserver `MIT` pour partage |
| `when_to_use` | Ignoré (serde ignore les champs inconnus) | **Recommandé** (routage agent Preview) | OS + dual | Pour un skill dual : **toujours** le remplir |
| `tools:` | Ignoré | Liste d’intents / tools Preview | OS + dual | Ne pas y mettre d’outils daemon (`memory_search`, …) |
| `tool_ref` | Extension daemon (raccourci vers un outil) | Non documenté OS | Daemon-only | |
| `agents` | Extension daemon (filtre type d’agent) | Non documenté OS | Daemon-only | |
| `compatibility` / `metadata` | Optionnel galerie Akasha_skills | Non requis OS | Daemon / portable doc | |
| `allowed-tools` / `version` | Présents dans certains skills hub ; **non** lus par `SkillFrontMatter` actuel | Non | Doc-only côté daemon | |
| Corps Markdown | Injecté à l’activation (`read_skill` / invocation) | Instructions agent Preview | Oui | Sections **par plateforme** si les outils divergent |
| `scripts/` `references/` `assets/` | Supportés à l’install GitHub (`install_skill`) | Preview charge **`SKILL.md` seul** | Daemon-only assets | Pour dual : garder la logique dans le Markdown |

### Procédure d’adaptation minimale

**OS → daemon**

1. Vérifier `name` + `description` présents.
2. Remplacer ou annoter les appels `tools:` Preview (`memory.recall`, `tasks.list`, `goal.complete`, …) par les outils daemon équivalents dans le **corps** (le front matter `tools:` peut rester pour Preview — le daemon l’ignore).
3. Copier vers `~/akasha/skills/<name>/SKILL.md` (ou `spec/skills/`) puis `/skills reload` / `POST /api/skills/reload`.

**Daemon → OS**

1. Ajouter `license: MIT`, `when_to_use:`, et `tools:` (intents Preview réellement disponibles).
2. Retirer ou isoler les étapes qui dépendent de plugins WASM / sidecars / `tools_policy` / canaux Telegram.
3. Copier vers `var/skills/<name>/SKILL.md` (Preview home) et redémarrer Preview — **pas** `share/skills/` (écrasé par update).

**Profil dual recommandé (arbitrage A8 provisional)**

Conserver **deux dialectes** + un sous-ensemble commun (`name`, `description`, `license`) + champs OS (`when_to_use`, `tools`) ignorés côté daemon. Un seul fichier peut donc être installé des deux côtés si le corps documente les deux jeux d’outils.

---

## Limites ABI (plugin ↔ module)

| Sujet | Daemon plugin | Module OS | Conséquence |
|-------|---------------|-----------|-------------|
| Runtime | Wasmtime (host plugin Akasha) | `module_rt` + `host_call` only | **Pas** d’échange de `.wasm` / `.aospkg` |
| Surface | Tools-only (± sidecar hors sandbox) | Dual-surface (tools + UI déclarative egui) | Une façade « plugin → module » = stretch A9 |
| Secrets | Vault daemon / policy | `secrets.get` interdit au guest ; services only via bridge | Ne jamais exposer de secrets bruts aux agents |
| Distribution | Catalogue `plugins.json` + trust-catalog CI | Catalogue local signé + **cap review** fail-closed | Pas de marketplace unifié en P9 |
| Packaging | Dossier `manifest.toml` + `plugin.wasm` | `.aospkg` (script ou rust) | Formats distincts |

**DoD P9** : documenter ces limites. **Hors scope** : unifier les ABI ou fusionner les binaires.

---

## Sibling bridge (mémoire / secrets) — référence daemon

Contrats live côté OS (Preview) ; **aucun client HTTP obligatoire** dans le daemon pour clôturer P9 must.

| Ressource | URL |
|-----------|-----|
| Guide | https://github.com/azerothl/akasha-os/blob/main/docs/sibling-bridge.md |
| Index schemas | https://github.com/azerothl/akasha-os/blob/main/docs/bridge/README.md |
| `mem.*` | https://github.com/azerothl/akasha-os/blob/main/docs/bridge/aos-proto-memory.json |
| `secrets.*` | https://github.com/azerothl/akasha-os/blob/main/docs/bridge/aos-proto-secrets.json |
| UI déclarative | https://github.com/azerothl/akasha-os/blob/main/docs/bridge/aos-proto-decl-ui.json |

Smoke optionnel (stretch) : `aos-bridged` sur `http://127.0.0.1:24710/v1` + header `X-Aos-From` (ne pas faire confiance à un champ JSON `actor` divergent).

---

## Inventaire catalogues (snapshot 2026-09-26)

Légende : **portable** = intention partageable via adaptation `SKILL.md` · **daemon-only** · **OS-only**.

### Skills daemon — [Akasha_skills](https://github.com/azerothl/Akasha_skills/tree/main/skills) (`skills.json`)

| Id | Classe | Motif |
|----|--------|-------|
| `morning-brief` (pilote monorepo `spec/skills/morning-brief`) | **portable** | Dual front matter ; voir § Pilote |
| `daily-digest`, `weekly-review`, `daily-project-checkin`, `project-pulse`, `memory-hygiene` | portable (concept) | Digests / mémoire — adapter tools OS (`memory.*`, `tasks.*`, `notes.*`) |
| `summarizer`, `calculator`, `date-time`, `json-yaml-utils`, `code-review`, `code-simplify`, `git-helper`, `file-manager`, `batch-processing`, `refactor-assistant`, `security-audit`, `skill-authoring`, `skill-discovery`, … | portable (doc / CLI générique) | Corps souvent `run_command` / fichiers — sous Preview, réécrire avec tools OS ou rester daemon |
| `home-assistant`, `debug-akasha`, `frontend-patterns`, `container-ops`, `plan-orchestrator`, `web-researcher`, … | **daemon-only** | Plugins HA, policy Akasha, navigateur daemon, orchestrateur agents |
| Life layer `morning_brief` / `overnight_pack` (schedules, pas skills hub) | **daemon-only** | API `/api/life/*`, Telegram notify — pair conceptuel du skill OS |

### Plugins daemon — [Akasha_plugins](https://github.com/azerothl/Akasha_plugins/tree/main/plugins)

| Id | Classe | Motif |
|----|--------|-------|
| `homeassistant`, `caldav-channel`, `matrix-channel` | **daemon-only** | Sidecar / connecteurs daemon |
| `graph`, `maps`, `simulation` | **daemon-only** | WASM tools-only ; pas de module UI OS équivalent publié |

### akasha-os community — [catalogue.yaml](https://github.com/azerothl/akasha-os/blob/main/community/catalogue.yaml)

| Id | Kind | Classe | Motif |
|----|------|--------|-------|
| `morning-brief` | skill | **portable** (source OS) + pilote dual monorepo | Seule entrée community skills à la sync |
| Modules community | module | **OS-only** | `.aospkg` + cap review ; pas d’équivalent plugin daemon |

Bundled Preview (`notes`, `tasks`, `canvas`, `ext-rt`, skills zip `share/skills/`) = **OS-only** (pas à réimplémenter dans le daemon).

---

## Pilote skill partagé : `morning-brief`

| | |
|--|--|
| Fichier | [`spec/skills/morning-brief/SKILL.md`](../../skills/morning-brief/SKILL.md) |
| Intention | Briefing local court (mémoire / tâches / notes) — **sans réseau** |
| Pair OS | [community/skills/morning-brief](https://github.com/azerothl/akasha-os/tree/main/community/skills/morning-brief) |
| Pair produit daemon | Life layer `POST /api/life/morning-brief` (schedule + notify) — **complémentaire**, pas un remplacement |

### Install daemon

```bash
# Depuis le monorepo (déjà sous spec/skills/ → chargé au démarrage / reload)
# ou copie manuelle :
mkdir -p ~/akasha/skills/morning-brief
cp spec/skills/morning-brief/SKILL.md ~/akasha/skills/morning-brief/
# Chat : /skills reload   ou   POST /api/skills/reload
```

Invocation : demander un « morning brief » / « brief matinal », ou `read_skill morning-brief`.

### Install akasha-os Preview

```bash
# Linux/macOS
mkdir -p ~/.local/share/agentos-preview/var/skills/morning-brief
cp spec/skills/morning-brief/SKILL.md ~/.local/share/agentos-preview/var/skills/morning-brief/
# Windows : %LOCALAPPDATA%\AgentOS-Preview\var\skills\morning-brief\SKILL.md
# Puis redémarrer Preview. Alternative catalogue : Install community `morning-brief`.
```

Le même fichier porte `when_to_use` + `tools:` (Preview) et `name`/`description` (daemon). Le corps sépare **Runtime: Akasha daemon** et **Runtime: akasha-os**.

Sync ultérieure vers le satellite `Akasha_skills` = optionnel (hors DoD monorepo).

---

## Hors scope / stretch (rappel)

- Fusion binaires · unification ABI · marketplace unique · client `aos-bridged` dans le daemon · façade plugin→module · « assistant as module » — voir roadmap P9 stretch / arbitrages A8–A11.

## Références

- Roadmap : [roadmap_v0.11.0.md](../releases/roadmap_v0.11.0.md) §P9  
- Guides OS : [write-a-skill](https://github.com/azerothl/akasha-os/blob/main/docs/write-a-skill.md) · [write-a-module](https://github.com/azerothl/akasha-os/blob/main/docs/write-a-module.md) · [sibling-bridge](https://github.com/azerothl/akasha-os/blob/main/docs/sibling-bridge.md)  
- Spec daemon skills : [33_agents_tools_orchestrator_skills.md](../../33_agents_tools_orchestrator_skills.md)
