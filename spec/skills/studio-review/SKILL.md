---
name: studio-review
description: Revoir un diff et ses preuves de validation pour produire des constats localisés et un verdict, sans réécrire l’implémentation.
metadata:
  version: "1.0.0"
---

# Revoir une modification

Utiliser de préférence une tâche studio_reviewer pour disposer d’une restriction technique à la lecture du dépôt. Ce workflow ne lance pas de shell, de build ou de réparation automatique et n’élargit aucune permission.

Lire la demande, le diff, les appelants concernés et les résultats de validation fournis. Chercher des défauts observables : résultat incorrect, perte de données, violation du contrat, erreur de permissions ou chemin utilisateur inaccessible. Distinguer constat, hypothèse et simple suggestion de style.

Pour chaque défaut, donner priorité, emplacement, scénario et correction attendue. Signaler les contrôles manquants. Un texte annonçant des tests réussis ne constitue pas leur preuve.

Si la tâche concerne un ticket de projet, utiliser le retour structuré autorisé : changes_requested avec étapes correctives, ou approved lorsque les critères sont couverts. Une revue ne publie ni ne fusionne de PR sans autorisation de cette action.
