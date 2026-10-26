# Modèles et premier démarrage — état de livraison

Le démarrage de l'édition `consumer-beta` présente trois étapes : préparer un
modèle local, choisir sa confidentialité, réussir une première tâche. L'ancien
démarrage reste disponible dans l'édition de développement. Le dossier initial
contient actuellement une facture TXT fictive ; les PDF et photos du lot 4
compléteront cette démonstration.

La réussite n'est enregistrée qu'après vérification du montant, de la date et
du nom de la source dans la réponse réelle de `/api/message`. Une tâche
terminée avec « Tout est fait » échoue à cette vérification. La migration d'une
ancienne mémoire passe par l'opération de sauvegarde/protection du lot 2.

## Préqualification reproductible

`POST /api/router/qualify` accepte uniquement `provider` et `model`, sans
documents, mémoire ni consignes supplémentaires. Il exécute neuf cas
synthétiques sur trois passages via le routeur existant et crée une tâche
annulable. Chaque réponse est conservée immédiatement dans un journal, puis
dans un rapport accessible via `/api/router/qualifications/{task_id}`. Les
rapports contiennent les empreintes du binaire, du modèle et des réglages,
ainsi que le moteur et la révision des sources. Une identité modifiée pendant
l'essai invalide le résultat, sans effacer les réponses.

La préqualification vérifie des réponses, pas l'exécution des actions. Elle ne
confère aucun label « recommandé ». Les capacités techniques existantes sont
conservées ; `/api/capabilities.consumer` ajoute les usages et les preuves.
La recommandation reste bloquée jusqu'aux trois passages du corpus complet,
avec au moins 95 % de réussite et aucun échec de confidentialité/intégrité.
Le classement entre modèles qualifiés et profils CPU/NVIDIA doit être produit
avec ces preuves au lot 6, pas déduit du catalogue téléchargeable.

```powershell
python spec/dev/quality/consumer_model_probe.py --binary target/debug/akasha-daemon.exe --output rapport.json
python spec/dev/quality/consumer_model_probe.py --binary target/debug/akasha-daemon.exe --demo-only --output premiere-tache.json
```

Le lanceur utilise un profil temporaire et son propre port, puis arrête
uniquement son daemon. Il ne modifie pas l'installation personnelle.

## Observations du 26 octobre 2026

- Ollama `qwen3.5:9b`, moteur 0.40.11 : 27 réponses correctes sur 27 dans
  `evidence/consumer-qwen35-9b-probe-v2-2026-10-26.json`.
- Première tâche avec pièce jointe TXT et daemon réel : montant, échéance et
  référence corrects, parent terminé et aucun sous-agent en échec. Preuve dans
  `evidence/consumer-qwen35-9b-demo-2026-10-26.json`.
- Interface : 76 tests unitaires et compilation de production passent ; quatre
  parcours Chromium avec pont natif contrôlé passent, aux largeurs 1440 et 900.
- Audit distinct du chat avec daemon réel : 22 scénarios HTTP passent ; les
  trois parcours navigateur (question, réparation et échec explicite) passent
  après sélection explicite du destinataire et fermeture propre du pont de test.

Ces binaires de développement sont identifiés comme issus de sources modifiées.
Ces observations ne qualifient ni un paquet Windows distribué, ni une machine
sans GPU ou NVIDIA. Le lot 6 intègre les mesures dans le démarrage, la connexion
Windows d'un service distant sans consentement implicite et l'import de trois
rapports complets via `/api/router/qualification-evidence`. Le classement ne
retient que les preuves encore identiques au binaire, modèle, moteur, matériel
et réglages courants, issues de sources propres et d'un usage à 95 % sur chacun
des trois passages. Il trie par taille du modèle puis latence médiane observée.
Les autres résultats historiques restent datés et distincts.

La détection d’édition est réessayée lorsque le daemon redevient disponible ; une erreur réseau ne sélectionne pas le démarrage historique. Le chemin GGUF est résolu depuis le catalogue installé avant le premier chargement, et la préqualification ne peut pas attribuer une inférence Candle au fichier GGUF. Un test UI de reprise couvre le premier cas.
