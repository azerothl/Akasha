# Contrats Studio v1 — #157

La requête LLM porte un champ optionnel `tools`. OpenAI, OpenRouter et Azure Chat Completions le transmettent ; les autres transports conservent l’adaptateur textuel. Une fonction native `studio_call` accepte une enveloppe versionnée. L’accumulation SSE attend les arguments complets, borne leur taille et préserve UTF-8. Un appel incomplet ou invalide n’est pas converti en commande. Aucun provider réel appelé pour cette livraison.

```text
TOOL_JSON: {"schema_version":1,"operation_id":"16e845ee-658d-49ea-8994-720709651257","tool":"read_file","arguments":{"path":"workspace:/src/main.ts"}}
```

`write_file` / `write_code` utilisent `{path,content,expected_version}`. `edit_file` ajoute `start_line,end_line` ; `apply_patch` utilise `content` pour le patch ; `search_replace` utilise `content` pour sa charge old/new ; `delete_file` ne prend pas de contenu. `expected_version` est le SHA-256 renvoyé par la lecture, ou `missing` pour une création. Les commandes utilisent `{program,args,cwd}`. MCP garde son objet d’arguments natif, validé par le schéma découvert. Les autres outils dispatchés utilisent `{argv:[...]}` avec leurs contrôles spécialisés existants. Les outils de workflow internes (délégation, skills, todos, question utilisateur) conservent `TOOL:` et ne sont pas annoncés comme fonctions natives dans cette tranche.

Les anciens appels restent acceptés. Pour les mutations de fichiers couvertes ci-dessus, le daemon les adapte à un identifiant d’opération et exige une lecture préalable d’un fichier existant. Un nouveau fichier utilise implicitement `missing`. Une version périmée est refusée. La lecture Studio utilise le disque, pas une copie virtuelle antérieure. Le contrôle est sérialisé entre outils Studio d’un même workspace ; il ne verrouille pas les éditeurs ou processus externes au daemon.

Le résultat JSON contient `schema_version`, `operation_id`, `tool`, `success`, `error`, `output`, `file_version`, `exit_code`. Les commandes synchrones renseignent leur véritable code quand un processus a été exécuté ; une absence de code reste `null`. Les sorties sont bornées à 64 Kio. Le journal SQLite `studio_operations` conserve un digest d’arguments, l’état `started` avant exécution et le résultat `completed`. Réutiliser un ID avec d’autres arguments est refusé ; réutiliser le même appel terminé retourne son résultat sans effet supplémentaire ; un état `started` ne se rejoue pas. Les permissions et l’état de la tâche sont contrôlés avant restitution.

Les mutations via shell, rename/move, connecteurs ou plugins gardent leurs garanties propres et ne bénéficient pas du contrôle de version fichier ci-dessus. Une sandbox OS et une atomicité avec des écrivains externes ne sont pas fournies par ce contrat.

## Validation

`cargo check -p akasha-daemon --no-default-features --features embedded` sous Windows. Pas de nouveaux tests ni de suites locales exécutées. Recettes à réaliser avant clôture : native streaming/non-streaming et fallback, UTF-8 fractionné, JSON tronqué, conflit de fichier, création concurrente, ID dupliqué/différent, refus de rôle et annulation pendant attente.

Référence officielle : <https://developers.openai.com/api/docs/guides/function-calling>.
