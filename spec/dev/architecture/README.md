# Carte d’architecture Akasha

Vue interactive des crates, du runtime (gateway → Main Agent → orchestrateur / conversation), du routeur LLM, de la mémoire 4 couches, des outils / vault / plugins, et des flux multi-canaux.

| Fichier | Usage |
|---------|--------|
| [architecture-diagram.html](architecture-diagram.html) | Diagramme standalone (ouvrir dans un navigateur) : nœuds, arêtes, panneau Flows, survol / clic, filtre |
| [architecture-graph.json](architecture-graph.json) | Graphe `{ nodes, edges, flows: [{ steps }] }` pour agents / outillage |

## Régénération à chaque release (recommandé)

**À chaque bump de version produit** (`X.Y.Z` / tag `vX.Y.Z`), régénérer ou mettre à jour ces deux fichiers pour qu’ils reflètent le code et les specs de la release :

1. Relire les frontières de crates (`core/CRATE_BOUNDARIES.md`), l’archi agents (`spec/05_agent_architecture.md`), gateway (`spec/48_gateway_layer.md`), mémoire (`spec/47_memory_4_layers.md`), LLM router (`spec/32_llm_router_architecture.md`), et les modules sous `crates/akasha-daemon/src/`.
2. Mettre à jour `architecture-graph.json` (nœuds, arêtes, flows).
3. Resynchroniser les données embarquées dans `architecture-diagram.html` (bloc `<script id="arch-data">`) pour que le HTML reste autonome.
4. Vérifier en local : ouvrir le HTML, sélectionner au moins un flow, contrôler hover / détail / filtre.
5. Committer les deux fichiers **dans le même PR de release** (ou juste après `scripts/sync-release-version.py`), avant le tag.

Checklist release (voir aussi [AGENTS.md](../../../AGENTS.md) § Release version alignment) :

- [ ] Version syncée (`sync-release-version.py`)
- [ ] **Carte d’architecture régénérée** (`spec/dev/architecture/`)
- [ ] Tag `vX.Y.Z` poussé
