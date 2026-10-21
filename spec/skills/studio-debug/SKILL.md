---
name: studio-debug
description: Diagnostiquer une erreur reproductible de code, de build ou d’exécution à partir du message exact et des journaux du projet Code Studio.
metadata:
  version: "1.0.0"
---

# Diagnostiquer un échec

Conserver le message exact, l’opération, les entrées et l’environnement pertinents. Identifier un scénario borné qui reproduit le problème. Si l’exécution exige un service, une donnée ou une permission absente, décrire ce prérequis ; ne pas inventer un résultat.

Suivre les données depuis l’entrée jusqu’au point d’échec. Comparer attendu et observé, puis isoler la cause avant de modifier le code. Préserver les fichiers utilisateur. Un rôle de lecture seule peut diagnostiquer avec les outils de lecture et doit laisser la correction à une tâche autorisée.

Après correction autorisée, rejouer le même contrôle et conserver son code de sortie ou sa preuve observable. Limiter les tentatives à des hypothèses distinctes ; après trois essais identiques sans nouvelle preuve, expliquer le blocage et la prochaine information nécessaire.
