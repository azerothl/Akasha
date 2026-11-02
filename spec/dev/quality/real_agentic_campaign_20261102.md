# Campagne agentique réelle du 2 novembre 2026

Les cinq scénarios passent avec **Qwen3.5:9b sur CPU et NVIDIA**, puis passent à nouveau en comparaison de leurs références mesurées, sans condition de non-publication (`no_go`). Les quatre rapports JSON voisins conservent les versions du runner, empreintes des exécutables, digest du modèle, configuration de génération, matériel, latences, statuts des tâches et reçus d’outils. Les déclarations de l’assistant ne suffisent pas aux contrôles.

## Protocole et résultats

- Appel effectif de `memory_store` avec reçu réussi.
- Refus d’une écriture interdite : reçu de refus et fichier témoin inchangé.
- Redémarrage réel du daemon, puis recherche du marqueur privé et réponse exacte issue de la mémoire.
- Fournisseur primaire indisponible, puis repli effectivement observé vers le modèle local attendu.
- Création d’un rappel, vérification du texte intégral et de sa règle persistés, pause, reprise, exécution avec réponse exacte, annulation et disparition effective.

Chaque passage utilise un profil synthétique isolé. La mémoire est vérifiée chiffrée avec SQLCipher avant les mesures ; les embeddings fonctionnent hors ligne. Les journaux et bases temporaires bruts ne sont pas publiés. Aucun compte public, document personnel ou installation existante n’est modifié.

Machine Windows 11 (build 26220), AMD64, 16 processeurs logiques, NVIDIA RTX 4080 SUPER 16 Gio. Ollama local observé pendant la campagne : **0.40.13**. Modèle et digest exacts dans les reçus ; température 0, contexte 16384, sortie maximale 2048, raisonnement désactivé. Le CPU force `num_gpu=0`, NVIDIA demande le déchargement GPU et contrôle la VRAM réellement observée. Daemon de développement compilé avec `--no-default-features --features embeddings-tract,akasha-store/memory-sqlcipher` ; ce test des outils génériques ne remplace pas la qualification du paquet bêta, qui restreint volontairement les outils avancés.

Les références ont été mesurées avec les révisions du runner `d37a8a2` (CUDA) et `9ed8e5b` (CPU). La comparaison utilise `f9a214c` et le daemon construit à `9ed8e5b`. Le champ `commit` désigne la révision du runner ; les exécutables sont identifiés séparément par leur SHA-256. Les deux comparaisons acceptent les latences selon les limites versionnées, sur le même matériel et le même digest de modèle.

## Reproduction et limites

Suivre [le runbook des évaluations](../../runbooks/04_evals.md) : initialiser une référence avec `--record-baseline` seulement depuis un arbre propre, la versionner, puis relancer sans cette option. Une référence échouée, un autre modèle, un autre digest ou un autre matériel ne donne pas une comparaison valide. `--evidence-dir` conserve facultativement des données synthétiques **localement** pour diagnostiquer un échec.

Les essais exploratoires en échec ne sont pas des références : ils ont révélé notamment la divergence entre exécution manuelle et automatique du rappel, la perte de son texte, l’annulation avec argument nommé et des réparations de transport incomplètes. Les contrôles ont été conservés ou renforcés pendant ces corrections.

Ces résultats constituent la suite minimale de l’issue #143 et non un label général « modèle recommandé ». Ils ne démontrent ni 95 % sur le corpus consommateur de 50 cas, ni trois passages consécutifs de qualification produit. Les tests déterministes Python du runner restent exécutés séparément en CI et ne comptent pas comme essais avec modèle réel.
