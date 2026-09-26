# Extensions Akasha vs akasha-os

Akasha (assistant / daemon) et [akasha-os](https://github.com/azerothl/akasha-os) (Preview) restent **deux produits**. Ils partagent des idées d’extensions, pas le même installateur ni la même ABI.

| Chez Akasha (ce guide) | Chez akasha-os Preview | À retenir |
|------------------------|------------------------|-----------|
| **Skill** — dossier + `SKILL.md` (Agent Skills), galerie [Akasha_skills](https://github.com/azerothl/Akasha_skills) | **Skill** — `var/skills/<id>/SKILL.md` ([write-a-skill](https://github.com/azerothl/akasha-os/blob/main/docs/write-a-skill.md)) | Même idée « recette Markdown » ; champs YAML un peu différents |
| **Plugin** — WASM + éventuel sidecar ([Akasha_plugins](https://github.com/azerothl/Akasha_plugins)) | **Module** — paquet `.aospkg` + UI + cap review ([write-a-module](https://github.com/azerothl/akasha-os/blob/main/docs/write-a-module.md)) | **Pas** interchangeables |
| Canaux, Life layer, Companion | Hors noyau OS | Rester côté daemon |

Il n’y a **pas** de marketplace unique. Un skill « portable » (ex. pilote `morning-brief` dans le dépôt) peut être copié des deux côtés après lecture de la note d’interop.

Pour contributeurs : [akasha-os-sibling-skills-modules.md](../../spec/dev/integrations/akasha-os-sibling-skills-modules.md) · bridge OS [sibling-bridge](https://github.com/azerothl/akasha-os/blob/main/docs/sibling-bridge.md).

Retour : [Skills, plugins et canaux](extensions.md).
