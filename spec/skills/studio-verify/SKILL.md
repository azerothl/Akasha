---
name: studio-verify
description: Vérifier une modification Code Studio contre des critères explicites en séparant contrôles exécutés, échecs, prérequis absents et validation manuelle.
metadata:
  version: "1.0.0"
---

# Vérifier une modification

Lire la demande, les critères d’acceptation, les commandes du projet et le diff concerné. Choisir une preuve observable pour chaque critère : fichier/contenu, résultat de commande, réponse API ou opération UI réelle.

Précondition d’exécution : rôle et politique autorisant la commande. Les scripts de build/test peuvent modifier le dépôt ; ne pas les lancer depuis un rôle planner/reviewer. Utiliser une tâche QA ou d’implémentation autorisée et limiter les données/services touchés au scénario demandé.

Conserver commande, répertoire, code de sortie, erreurs et prérequis pertinents. Marquer explicitement tout contrôle non lancé. Un contrôle manuel ou une appréciation modèle reste manuel ; il ne devient pas une preuve mécanique.

Comparer le résultat au contrat demandé. En cas de réponse vide, timeout, limite ou succès partiel, conserver les éléments utiles et signaler le motif. Ne déclarer la demande vérifiée que pour les critères effectivement couverts.
