# Génération autonome Code Studio : diagnostic et contrat

## Défauts corrigés

- Les rôles d'implémentation `studio_*` utilisent `code_generation`, avec le repli conversation existant si aucune route de code n'est configurée.
- La sélection explicite d'un modèle privilégie les paramètres de code. Elle ne dépend plus de l'ordre aléatoire d'une HashMap contenant aussi les routes utilitaires.
- Le budget de réponse par défaut des tâches Studio est de 8 192 tokens ; les limites explicites du routeur et de l'objectif restent appliquées.
- Les fonctions natives Ollama sont annoncées avec leurs noms et leurs arguments propres. L'historique utilise les mêmes noms ; le runtime conserve son enveloppe versionnée interne et vérifie le catalogue effectif.
- Pour les Modelfiles Qwen 3.5 explicitement limités à `{{ .Prompt }}`, un adaptateur texte fournit les rôles ChatML et le transport d'outils Qwen. Les paramètres multilignes sont conservés et une génération incomplète ne produit aucun appel exécuté. Référence : [gabarit officiel Qwen](https://huggingface.co/Qwen/Qwen3.5-9B/blob/main/chat_template.jinja).
- Un rejet du protocole natif peut déclencher jusqu’à trois reformulations bornées; un rejet JSON explicite par le fournisseur (ex. `error parsing tool call`) n’en déclenche qu’une. Le détail brut du JSON fournisseur n’est jamais réinjecté dans le prompt ni dans le ticket; un second rejet arrête la tâche avec un diagnostic exploitable. Aucun appel de la génération rejetée n’est exécuté ou rejoué. Pour un objectif, le budget de réponse est débité comme estimation conservatrice lorsque le fournisseur ne retourne pas de reçu de consommation.
- Les consignes natives sont séparées des exemples historiques : les schémas du fournisseur décrivent les arguments, sans doubler tout le catalogue dans le texte système.
- Un objectif encore actif fournit son état après les outils ; aucune relance artificielle n’est ajoutée après sa réussite. Cela conserve aussi le catalogue des gabarits qui l’attachent seulement au dernier message utilisateur.
- Les rappels de demandes longues dans les résultats sont bornés, tandis que la demande complète reste dans le message initial. Le délai du premier résultat natif respecte le délai fournisseur configuré, car certains fournisseurs mettent les arguments en tampon jusqu’à la fin de l’appel.
- Les consignes ne demandent plus de fabriquer un UUID d'opération ni de privilégier la syntaxe historique lorsque le transport natif est disponible.
- Les commandes ferment leur entrée standard et reçoivent `CI=true`. Les assistants `eslint --init` et `npm init` sans confirmation non interactive sont refusés avant création de processus. Sous Windows, le processus et ses descendants sont arrêtés sur délai dépassé ou annulation de l'appel ; le processus direct utilise aussi `kill_on_drop`.
- Les candidats d'écriture des manifestes JSON stricts à la racine du projet Studio enregistré sont analysés avant modification du disque, y compris les éditions partielles. Les configurations JSONC, les fixtures imbriquées et les outils génériques hors Studio sont exclus de ce contrôle.
- Un objectif ne consomme plus de cycle de vérification après une simple lecture ni avant l'existence des fichiers explicitement requis. Son plafond de tours d'outils est distinct (10 par cycle autorisé) et reste borné par ses budgets de temps et de tokens.

- La fenêtre active est bornée avant envoi à Ollama : les anciens groupes d’appels avec reçus complets sont résumés, sans modifier l’historique durable. Les consignes humaines, règles du dépôt et résultats incertains restent intacts. Le dernier groupe conserve ses identités et reçus factuels. La réserve de sortie peut diminuer pour conserver un gros lot de lectures ; si celui-ci dépasse encore la fenêtre, les lectures ordinaires volumineuses sont présentées en extraits explicitement signalés, avec leur empreinte et une demande de relecture des lignes omises avant modification. Une impossibilité restante est signalée explicitement.

- Un objectif encore non vérifié exige un véritable appel d’outil : une réponse finale anticipée ne le termine pas. Pour les réponses natives Ollama qui expriment l’appel complet comme objet JSON standard, un adaptateur strict accepte uniquement les clés `name` et `arguments`, un nom annoncé et une génération terminée. Le résultat passe ensuite dans le contrat typé normal ; les exemples dans la prose, générations coupées et champs inconnus sont rejetés.
- Après une stagnation en lecture seule ou en commandes échouées d’un objectif déjà vérifié au moins une fois, les outils de modification déjà autorisés sont présentés en priorité, sans retirer les lectures et commandes nécessaires à la réparation. Les profils en lecture seule restent en lecture seule. Les rappels n’imposent plus le format historique ou le format XML d’un autre modèle.
- Le recentrage conserve tous les outils déjà autorisés, notamment les lectures, recherches et commandes de validation. Il ne doit pas refuser `read_file` ou `run_command` pendant une réparation. Les rejets natifs indiquent les noms demandés et le catalogue effectif, sans exposer les arguments.
- La reprise n’injecte plus les anciennes sorties complètes dans son rappel : elle conserve les reçus factuels, versions et résultats incertains, tandis que les sorties complètes restent accessibles dans le registre durable.

- Les appels natifs en tampon ont leur propre délai fournisseur fini. Leur lancement constitue une activité réelle du worker : le watchdog de démarrage à 180 secondes ne les interrompt plus avant ce délai. Cette activité n’est ni une modification de fichiers ni une réussite du but. Les erreurs d’opérations n’entraînent plus une seconde exécution automatique de la même vérification.

- Les gabarits Mistral qui insèrent les résultats d’outil comme JSON reçoivent un contenu JSON valide. L’adaptation conserve les identités, sorties et versions ; elle ne modifie pas le journal durable.
- Une édition `search_replace` qui ne change aucun octet échoue explicitement avec `edit_no_change`. Elle ne compte plus comme progrès et demande la relecture du texte exact avant une nouvelle tentative.
- La réduction de contexte conserve ensemble les derniers groupes de fichiers et le dernier diagnostic de compilation en échec, jusqu’à un diagnostic d’objectif plus récent.

- Sous Windows, les lanceurs `.cmd` et `.bat` des commandes autorisées partagent la même identité exacte que le nom sans extension, comme `.exe`. Les listes de refus utilisent la même normalisation et gardent leur priorité ; aucun droit n’est ajouté aux politiques explicitement vides. Cela évite le refus injustifié de `npm.cmd` après un appel de `npm`.

- Les résultats natifs Ollama transmettent `tool_name`, résolu par l’identifiant de l’appel associé après dépliage du contrat Studio. Les gabarits GPT-OSS utilisent ce nom dans le rôle du résultat ; sans lui, le modèle recevait des résultats attribués à une fonction vide. Un résultat sans appel connu ne reçoit aucun nom fabriqué.

- Le rappel d’objectif affiche la présence observée de chaque fichier requis, sans la confondre avec une validation fonctionnelle. Le modèle conserve ainsi les prérequis manquants après réduction du contexte et reçoit une consigne de ne pas réinstaller des dépendances déjà présentes.
- Les arguments natifs distinguent désormais le contenu complet de `write_file`, le texte de plage de `edit_file` et les hunks numérotés de `apply_patch`. Les rejets de patch expliquent le format requis.
- Le Kanban distingue `parent_ticket_id` (regroupement et suivi, sans blocage du dispatch) de `depends_on_ticket_ids` (prérequis réels). La création agentique accepte une `creation_key` stable et réutilise le ticket canonique pour les répétitions; les anciens tickets enfants de la première demande sont progressivement rattachés au parent sans le transformer en prérequis.
- L’orchestrateur reconnaît les tickets enfants par leur relation parent et leur ancien lien de dépendance pour préserver les tableaux existants. Une création sous un parent global terminé le rouvre pour que les nouvelles tâches soient suivies jusqu’à leur revue.

## Ce que les validations prouvent

Les tests Rust vérifient les contrats, les permissions et la reprise bornée. Le test réel `native-probe.txt` vérifie écriture puis relecture avec Qwen 3.5 ; il a également exercé la reformulation automatique d'un appel mal formé.

Une compilation et des fichiers présents ne prouvent pas qu'un jeu fonctionne. La recette Bomberman part donc d'un projet sans sources applicatives et ajoute uniquement des recettes indépendantes du moteur et du navigateur. Les sources du jeu sont écrites par l'agent. La recette vérifie notamment le menu, le nombre d'ennemis, les déplacements, la stabilité du terrain, l'IA, la bombe, la pause, la défaite et les commandes mobiles. Son empreinte est contrôlée en dehors du projet pour détecter une modification du test.

## Limites

Le transport Qwen ajouté concerne le texte et le gabarit brut explicitement reconnu ; il ne constitue pas une validation multimodale. Les permissions explicites, profils, refus et contrôles de version continuent de s'appliquer. Une réussite mécanique ne doit jamais être présentée comme une validation fonctionnelle complète sans les résultats du parcours réel.

Les paramètres du routeur local et les journaux de recette appartiennent aux données d'exécution ; ils ne sont pas des valeurs universelles adaptées à tous les GPU ou modèles.

## Recette réelle du 21 octobre 2026 : validation incomplète

Le daemon courant passe 582 tests (un ignoré), le transport LLM 92 et les outils 38. L’intégration réelle de création de projet passe ; sa recette LLM optionnelle est ignorée et ne constitue pas une preuve de génération.

Les quatre derniers essais réels enregistrent zéro refus d’accès dans les reçus d’opérations. Les sources applicatives sont toutes écrites par les agents ; seuls les scripts de recette sont fournis par l’opérateur. Une phase de compilation dédiée a réussi dans le projet précédent, sans prouver le jeu.

La dernière création autonome, projet `35dde7ae-fe46-448f-8c1d-50bc7dc28ef2`, reste en échec : TypeScript signale `cell` hors portée dans `src/game.ts` (lignes 151 et 156), et la recette indépendante du moteur obtient 6 réussites et 8 échecs. La tentative Qwen avec raisonnement moyen termine sur génération incomplète ; celle sans raisonnement termine sur répétition d’appels sans correction. Le parcours navigateur et un aperçu jouable de cette génération ne sont donc pas validés. L’ancien aperçu manuel sur le port 5181 ne doit pas être utilisé comme preuve d’autonomie.

Aucune source de jeu n’a été corrigée manuellement pour faire passer cette recette. Les budgets et assertions restent appliqués ; le travail ne peut pas être déclaré terminé sur la seule réussite des tests du harnais.
