# Évaluation — navigateur hybride vision + DOM (UI-TARS / Midscene)

Document de **cadrage** (pas une dépendance runtime). Il sert à décider si l’on ajoute une couche de **grounding visuel** au-delà de Playwright (DOM) déjà présent dans Akasha.

## Contexte

- **Akasha aujourd’hui** : `browser` = Playwright (sélecteurs, snapshot texte, screenshot). Les captures peuvent être renvoyées au **modèle multimodal** au tour suivant (`spec/39_browser_automation.md`).
- **UI-TARS / Agent TARS** : combine souvent **VLM + coordonnées** ou stratégies hybrides DOM / vision ([UI-TARS-desktop](https://github.com/bytedance/UI-TARS-desktop), [Midscene](https://github.com/web-infra-dev/midscene) côté doc communautaire).

## Critères de décision

| Critère | Question |
|---------|----------|
| Cas d’usage | Les écrans cibles sont-ils dominés par canvas, shadow DOM difficile, ou anti-automation où le DOM ne suffit pas ? |
| Coût | API cloud vision vs modèle local GPU ; latence par étape. |
| Sécurité | Service tiers : données d’écran, conformité, résidence des données. |
| Maintenance | Dépendance npm supplémentaire, breaking changes, licence. |

## Prochaines étapes si validation produit

1. Prototype **manuel** : enchaîner `browser screenshot` + modèle vision déjà routé (pas de nouveau binaire) et mesurer la qualité sur 5 flux réels (formulaire, SPA, preview Code Studio).
2. Si insuffisant : évaluer **Midscene** ou une API **UI-TARS** sur un sous-ensemble d’actions (clic coordonnées) avec traduction vers `browser click` Playwright.
3. Documenter choix et limites dans `spec/39_browser_automation.md`.

## Sortie

Décision **retenue / non retenue** + lien vers issues ou ADR si implémentation.
