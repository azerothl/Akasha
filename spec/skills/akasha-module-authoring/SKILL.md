---
name: akasha-module-authoring
description: Concevoir, créer ou intégrer dans Akasha un module applicatif lorsque l’utilisateur le demande, en choisissant le bon type d’extension et en respectant le catalogue, l’installation et les limites de sécurité.
metadata:
  version: "1.0.0"
---

# Créer un module intégrable à Akasha

Utiliser ce workflow lorsqu’un utilisateur demande explicitement de créer, ajouter ou intégrer un module dans Akasha. Une question sur la faisabilité ne vaut pas demande de création. Ne crée jamais un module ni ne lance son installation de ta propre initiative.

## Choisir le bon type d’extension

Avant de modifier le dépôt, lire ses `AGENTS.md` applicables et vérifier l’architecture présente dans les fichiers concernés. Akasha a plusieurs mécanismes d’extension distincts :

| Besoin utilisateur | Type adapté |
|---|---|
| Ajouter une application avec une interface accessible depuis Akasha | Module applicatif Akasha, inscrit au catalogue compilé et présenté dans l’onglet Modules |
| Ajouter un outil exécuté par les agents | Plugin Akasha (contrat WASM et permissions), pas un module d’interface |
| Ajouter une procédure ou des connaissances à charger à la demande | Skill `SKILL.md` |
| Ajouter une extension `.aospkg` à akasha-os | Module akasha-os, avec son propre runtime et son propre contrat |

Ne confonds pas ces formats et ne crée pas de nouveau runtime de modules dynamiques pour contourner le catalogue existant. Si le terme « module » ne permet pas de déterminer la surface souhaitée, demande une précision avant d’implémenter.

## Intégrer une application Akasha

Prendre Code Studio comme exemple d’intégration, puis suivre les conventions actuelles du dépôt plutôt que copier aveuglément ses valeurs. Examiner au minimum :

- `crates/akasha-daemon/src/module_catalog.rs` pour les métadonnées, la détection de disponibilité et les lancements autorisés ;
- `crates/akasha-daemon/src/api_routes_modules.rs` pour la découverte et les opérations exposées ;
- `apps/akasha-ui/src/components/ModulesPanel.tsx` et son CSS pour l’état, les explications et les actions visibles ;
- `apps/akasha-ui/src-tauri/src/lib.rs` pour les opérations natives d’installation et l’ouverture des URLs ;
- les routes, la navigation et les locales de l’interface si le module nécessite sa propre vue ;
- `spec/user_guide.md` pour expliquer à l’utilisateur comment l’obtenir et le lancer.

Adapter les points d’intégration nécessaires au module demandé. Tous les appels shell, processus, URLs et identifiants doivent rester déterminés par le code et explicitement autorisés. Le daemon ne lance qu’un ID connu et un point d’entrée installé attendu ; l’application écoute seulement sur loopback si elle expose un serveur local. Ne transforme jamais un nom de paquet, une commande, un chemin, un port ou une URL provenant de la conversation en exécution libre.

L’installation est une action visible et déclenchée par l’utilisateur dans l’interface Akasha. Les commandes natives doivent utiliser un paquet et une version explicitement approuvés dans le code, sans paramètre arbitraire transmis par l’agent. Ne prétends pas qu’un module est disponible avant d’avoir vérifié ses prérequis, son installation, sa version et son état d’exécution. Affiche une explication exploitable lorsque Node.js, npm, le paquet ou le port requis manque.

Le catalogue actuel est compilé dans Akasha et contient une liste blanche de modules de première partie. Ajouter un module au code source l’intègre à cette version d’Akasha ; cela ne crée pas à lui seul un marketplace, une installation dynamique tierce ou une compatibilité `.aospkg`. Pour une demande de distribution tierce, identifier d’abord les besoins de signature, provenance, mises à jour, permissions et révocation ; ne pas installer du code tiers sans un catalogue de confiance et un parcours d’approbation appropriés.

## Contrôles avant livraison

Pour chaque module, vérifier et rapporter les éléments applicables :

1. Le choix du type d’extension correspond au besoin et ses identifiants sont uniques et validés.
2. Le catalogue décrit clairement le nom, la fonction, la source, la version, les prérequis, l’état installé/en cours et l’action possible.
3. L’API n’expose que les actions nécessaires ; la découverte est en lecture seule et chaque lancement est limité à un ID répertorié.
4. L’interface présente des états vides, indisponibles, installés, en cours et en erreur avec des explications utiles, ainsi que des libellés localisés.
5. Les opérations natives utilisent des valeurs fixes, gèrent les erreurs et ne font pas confiance aux données de l’agent ou du serveur pour exécuter une commande.
6. La version annoncée, le paquet, le point d’entrée, la source et la compatibilité runtime concordent.
7. La documentation et les commandes de validation du dépôt couvrent les parcours modifiés. Distinguer les contrôles exécutés de ceux qui restent à faire.

Conserver les changements utilisateur sans rapport. Ne pas installer ni démarrer le module pendant le développement, sauf si l’utilisateur le demande explicitement. Ne pas déclarer le module livré, publié ou disponible avant que les preuves correspondantes existent.
