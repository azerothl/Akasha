# Comparaison Vec et VecDeque en Rust

## Résumé des données probantes
La documentation officielle distingue `Vec` comme un tableau dynamique contigu et `VecDeque` comme une file double terminale (deque) implémentée via un tampon d'anneau. Les deux structures offrent un accès par indice en O(1), mais leurs performances diffèrent lors des insertions/suppressions aux extrémités.

## Organisation mémoire
Selon [std/vec/index.html](https://doc.rust-lang.org/std/vec/index.html), `Vec<T>` est un tableau contigu avec des contenus alloués sur le tas (`heap-allocated`). Cette structure garantit la localité mémoire nécessaire pour une performance optimale lors de l'accès séquentiel.

En revanche, [std/collections/vec_deque/index.html](https://doc.rust-lang.org/std/collections/vec_deque/index.html) décrit `VecDeque` comme étant implémenté avec un "growable ring buffer" (tampon d'anneau grandissant). Cette architecture permet une réutilisation de l'espace mémoire en masquant les éléments supprimés, évitant ainsi les allocations fréquentes lors des opérations aux deux extrémités. La documentation précise que pour `VecDeque`, les éléments ne sont pas requis d'être copiables (*copyable*), et la file sera `sendable` si le type contenu l'est.

## Accès par indice et Complexités amorties
Les deux structures partagent une complexité d'accès en O(1) via indexation, confirmée pour `Vec` [std/vec/index.html](https://doc.rust-lang.org/std/vec/index.html) et pour `VecDeque` [std/collections/vec_deque/index.html](https://doc.rust-lang.org/std/collections/vec_deque/index.html).

Cependant, les opérations d'ajout et de retrait présentent des divergences majeures :
*   **Ajout/Retrait à la fin** : `Vec` offre un coût amorti O(1) pour le `push` (fin) et le `pop` (fin) [std/vec/index.html](https://doc.rust-lang.org/std/vec/index.html). Une redimensionnement occasionnel prendra O(n)*, où n est la capacité.
*   **Ajout/Retrait aux deux extrémités** : `VecDeque` offre un coût amorti O(1) pour les insertions et suppressions depuis les deux extrémités [std/collections/vec_deque/index.html](https://doc.rust-lang.org/std/collections/vec_deque/index.html).
*   **Opérations à l'opposé de la croissance** : La documentation mentionne que les opérations d'ajout occasionnellement nécessitent un redimensionnement. De plus, les suppressions ne sont pas amortisées car les collections de Rust ne rétrécissent jamais automatiquement [std/collections/index.html](https://doc.rust-lang.org/std/collections/index.html).

## Usages et Limites
Le choix dépend du profil d'accès :
1.  **Utiliser `Vec`** lorsque l'ajout se produit principalement à la fin de la séquence, ou pour des accès séquentiels lourds profitant de la contiguïté mémoire stricte.
2.  **Utiliser `VecDeque`** lorsqu'il est nécessaire d'accéder ou de modifier les éléments aux deux extrémités fréquemment avec une performance constante.

La documentation note que `VecDeque` possède des méthodes spécifiques comme `as_slices()` retournant `(&[T], &[T])` pour séparer la file en parties front et arrière, utile pour l'optimisation d'accès [std/collections/struct.VecDeque.html](https://doc.rust-lang.org/std/collections/struct.VecDeque.html). L'API de conversion vers un vecteur est disponible via `into()`.

## Limites et Confiance
*   **Limites** : Aucune des deux structures ne réduit automatiquement sa capacité après suppression. Les coûts O(n)* indiqués dans les tableaux de la documentation [std/collections/index.html](https://doc.rust-lang.org/std/collections/index.html) s'appliquent uniquement lors du redimensionnement nécessaire, pas à chaque opération.
*   **Confiance** : Les complexités sont basées sur des analyses théoriques fournies par la bibliothèque standard de Rust (2026). Les performances réelles peuvent varier selon le taux d'allocation et la fragmentation du tas.

## Conclusion
`Vec` reste la structure préférée pour les séquences linéaires classiques, tandis que `VecDeque` est optimisé pour les files de données où l'accès aux deux bouts est critique. La documentation confirme que `VecDeque` maintient une performance O(1) amortie à tous les bouts, contrairement à `Vec` qui devient O(n) lors des insertions au début si la capacité doit être réallouée ou déplacée.