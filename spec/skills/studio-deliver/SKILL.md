---
name: studio-deliver
description: Préparer et livrer une modification Git demandée avec branche, commit, PR et état CI traçables, selon les actions externes autorisées.
metadata:
  version: "1.0.0"
---

# Livrer une modification

Lire le statut Git et la demande pour identifier les actions autorisées : commit local, push, ouverture de PR, publication ou fusion. Le skill ne donne aucune de ces autorisations. Préparer un résultat concret à examiner avant toute confirmation qui reste nécessaire.

Préserver les changements sans rapport et ne stage que les chemins du travail concerné. Lire le diff destiné au commit. Vérifier branche, dépôt distant et prérequis d’authentification sans afficher de secret.

Si une PR est demandée, relier chaque ticket concerné et préciser les contrôles réellement exécutés. Utiliser une référence de progression tant que les critères d’un ticket ne sont pas tous satisfaits ; ne pas fermer par simple existence de la PR.

Lire l’état CI de la révision publiée. Distinguer échec, attente, contrôle non exécuté et réussite. Une fusion nécessite son autorisation et les conditions demandées. Terminer avec les URL et l’état réel de livraison.
