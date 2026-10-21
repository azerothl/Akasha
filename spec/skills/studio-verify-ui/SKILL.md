---
name: studio-verify-ui
description: Vérifier une interface modifiée en naviguant réellement dans les parcours desktop et mobile et en conservant les erreurs et preuves visuelles de la tâche.
metadata:
  version: "1.0.0"
---

# Vérifier une interface

Vérifier le manifeste et les prérequis du navigateur. Démarrer le serveur de développement seulement si la tâche l’autorise. Si le navigateur n’est pas disponible, expliquer ce manque ; un examen de composants ne vaut pas une validation visuelle.

Suivre les parcours affectés sur une largeur desktop et une largeur mobile représentatives. Contrôler navigation, défilement, états chargement/vide/erreur, focus clavier et actions principales. Lire les erreurs console et réseau utiles au scénario.

Conserver captures, dimensions, URL et étapes avec la tâche. Identifier les mocks, services réels et données utilisées. Préserver les projets et sessions utilisateur existants ; aucune publication ne découle de cette vérification.

Après une correction autorisée, rejouer le parcours concerné. Rapporter ce qui a été observé et les états non couverts, sans présenter une capture isolée comme preuve de tous les parcours.
