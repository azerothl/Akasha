# Analyse comparative des collections séquentielles en Rust : Vec et VecDeque

## Résumé exécutif

Ce rapport évalue les caractéristiques de `Vec` et `VecDeque` basées sur la documentation officielle de la bibliothèque standard de Rust. Les deux structures sont des types de séquence offrant une indexation en O(1) et des insertions/suppressions amorties en O(1) aux extrémités [collections]. Cependant, leurs architectures diffèrent : `Vec` utilise un tableau contigu alloué sur le tas (heap-allocated contents), tandis que `VecDeque` utilise un tampon annulaire croissable (growable ring buffer) où les éléments ne sont pas nécessairement contigus en mémoire [vecdeque-struct]. La documentation précise que lorsque des complexités théoriques sont identiques, `Vec` est généralement plus rapide que `VecDeque` [collections]. Le choix dépend du compromis entre la nécessité d'accéder au début de la séquence et la préférence pour une allocation mémoire contigue.

## 1. Organisation mémoire et architecture interne

### Architecture du Vec
Le type `Vec<T>` est défini comme un tableau croissable avec des contenus alloués sur le tas [vec-index]. Sa structure repose sur une allocation continue en mémoire, permettant aux éléments d'être stockés dans des adresses consécutives. Cette contiguïté est inhérente à la définition du type : un pointeur `ptr: *mut T` alloué avec `Layout::array::<T>(capacity)` peut être converti directement en `Vec` via `Vec::<T>::from_raw_parts(ptr, len, capacity)` [vec-index].

### Architecture du VecDeque
Contrairement au `Vec`, le `VecDeque` est implémenté avec un « tampon annulaire croissable » (growable ring buffer) [vec_deque]. Cette architecture implique que les éléments ne sont pas nécessairement contigus en mémoire ; ils peuvent être dispersés dans plusieurs blocs d'allocation pour maintenir la logique du tampon circulaire. Pour accéder aux éléments comme une seule tranche (slice), il est impératif d'utiliser la méthode `make_contiguous`, qui fait pivoter le tampon pour rendre les éléments contigus et retourne une tranche mutable [vecdeque-struct].

**Note de limitation :** L'utilisation native du `VecDeque` sans appel à `make_contiguous` peut être problématique pour les opérations nécessitant une vue unique sur la mémoire, telles que le tri par pointeur externe ou l'envoi de tranches de données à des systèmes externes qui s'attendent à une contiguïté garantie.

## 2. Accès par indice et itération

### Complexité d'accès
Les deux structures offrent une complexité **O(1)** pour l'indexation, permettant un accès direct en temps constant :
*   Pour le `Vec`, l'indexation est O(1) [vec-index].
*   Le `VecDeque` possède également une indexation O(1) comme un vecteur [vec_deque].

### Méthodes d'accès spécifiques
Le `VecDeque` expose la méthode `get(index: usize)` qui retourne une option de référence vers l'élément à l'indice donné, où l'indice 0 correspond au front de la file [vecdeque-struct]. Bien que théoriquement identique en complexité, l'accès physique dans le `VecDeque` peut nécessiter des calculs d'index modulo la capacité du tampon, ce qui diffère de l'accès linéaire direct du `Vec`.

## 3. Ajout et retrait (Début et Fin)

### Opérations aux extrémités
Le `VecDeque` est conçu pour supporter des insertions efficaces aux deux extrémités de la séquence [collections]. Les opérations spécifiques sont :
*   **Ajout au front :** `push_front(val: T)` qui peut provoquer une réallocation et un copiage de tous les éléments si le tampon est plein [vecdeque-struct].
*   **Retrait du front :** `pop_front()` retire et retourne l'élément du front, retournant `None` si vide [vecdeque-struct].
*   **Ajout au fond :** `push_back(val: T)` avec les mêmes contraintes d'allocation que `push_front` [vecdeque-struct].
*   **Retrait du fond :** `pop_back()` retire et retourne l'élément du fond [vecdeque-struct].

### Comparaison de performance (Ajout/Retrait)
Selon la documentation, les deux structures ont des complexités amorties **O(1)** pour le `push` (fin) et le `pop` (fin) [vec-index]. Pour le `VecDeque`, les insertions et suppressions aux deux extrémités sont également O(1) amorties [vec_deque].

**Nuance importante :** Bien que la complexité théorique soit identique (O(1)), la documentation note explicitement : « Note that where ties occur, Vec is generally going to be faster than VecDeque » [collections]. Cela suggère que pour les opérations de base aux extrémités, le `Vec` peut surpasser le `VecDeque` en performance brute, bien que ce dernier soit nécessaire pour l'accès au début.

## 4. Complexités amorties et gestion de la capacité

### Croissance et réallocation
Les deux collections utilisent une stratégie d'allocation amortie. Lorsqu'une opération d'ajout nécessite un redimensionnement, cela prend **O(n)** temps [collections]. Pour atténuer ce coût, les collections allouent généralement un tableau substantiellement plus grand lors de la croissance afin de retarder le prochain redimensionnement.

### Gestion de la capacité
Le `VecDeque` expose des méthodes spécifiques pour gérer la capacité :
*   `capacity()` retourne le nombre d'éléments que le deque peut contenir sans réallocation [vecdeque-struct].
*   `reserve_exact(additional: usize)` réserve l'espace minimum pour ajouter des éléments, sans rien faire si la capacité est suffisante. La documentation avertit que l'allocateur peut donner plus d'espace que demandé, donc la capacité ne doit pas être considérée comme minimale [vecdeque-struct].
*   `shrink_to_fit()` réduit la capacité autant que possible, se rapprochant de la longueur actuelle, bien que l'allocateur puisse encore signaler qu'il y a de l'espace pour quelques éléments supplémentaires [vecdeque-struct].

**Note critique sur le retrait :** La documentation stipule : « Rust's collections never automatically shrink, so removal operations aren't amortized » [collections]. Cela signifie que supprimer des éléments ne réduit pas la capacité allouée ; l'amortissement ne s'applique qu'à l'allocation (croissance), pas à la libération de mémoire lors du retrait.

## 5. Tableau comparatif des complexités

Le tableau suivant synthétise les complexités temporelles pour les opérations clés, telles que documentées dans le manuel de référence [collections]. Le symbole `*` indique une complexité amortie ou des conditions spécifiques.

| Opération | Vec | VecDeque | Notes |
| :--- | :--- | :--- | :--- |
| **get(i)** (Accès par indice) | O(1) | O(1)* | Le `VecDeque` nécessite un calcul d'index modulo la capacité. |
| **insert(i)** | O(n - i)* | O(min(i, n - i))* | Les deux sont O(n) dans le pire cas (début ou fin). |
| **remove(i)** | O(n - i)* | O(min(i, n - i))* | Le coût dépend de la distance à l'extrémité libre. |
| **append(Vec(m))** | O(m)* | O(m)* | Ajout en fin de liste. |
| **push_front / push_back** | N/A (O(n) au front) | O(1)* | Le `Vec` est lent au front ; le `VecDeque` est constant aux deux bouts. |
| **pop_front / pop_back** | N/A (Impossible sans décalage) | O(1)* | Le `VecDeque` permet un retrait efficace du front. |
| **split_off(i)** | N/A | N/A | Opération spécifique aux queues. |

*Note : La documentation précise que « where ties occur, Vec is generally going to be faster than VecDeque » [collections].*

## 6. Cas d'usage et recommandations détaillées

### Quand utiliser Vec ?
La documentation recommande le `Vec` dans les cas suivants :
*   On souhaite collecter des éléments pour être traités ou envoyés ailleurs plus tard, sans se soucier des propriétés spécifiques des valeurs stockées [collections].
*   On veut une séquence d'éléments dans un ordre particulier et on n'ajoutera (ou presque) qu'à la fin [collections].
*   On veut une pile (stack) [collections].
*   On veut un tableau redimensionnable ou alloué sur le tas [collections].

### Quand utiliser VecDeque ?
Le `VecDeque` est recommandé lorsque :
*   On veut un `Vec` qui supporte des insertions efficaces aux deux extrémités de la séquence [collections].
*   On veut une file (queue) ou un double-ended queue (deque) [collections].

### Comparaison globale de performance
Le tableau de performance dans la documentation compare les opérations `get(i)`, `insert(i)`, `remove(i)`, `append(Vec(m))` et `split_off(i)` :
*   **Vec :** O(1) pour get/append, O(n-i)* pour insert/remove/split [collections].
*   **VecDeque :** O(1) pour get/append, O(min(i, n - i))* pour insert/remove/split [collections].

*(Note : Le symbole `*` dans la documentation originale indique généralement une complexité amortie ou des conditions spécifiques, mais le texte fourni ne détaille pas explicitement ce que signifie l'étoile ici, se contentant de lister les complexités.)*

**Gaps identifiés :**
1.  La documentation mentionne que `VecDeque` est « generally going to be faster than LinkedList » pour les opérations intermédiaires [collections], mais ne fournit pas de données comparatives directes entre `Vec` et `VecDeque` au-delà de la remarque générale que `Vec` est souvent plus rapide. Une investigation empirique serait nécessaire pour valider ce « generally » dans des cas d'usage spécifiques (ex: accès aléatoire fréquent vs écriture séquentielle).
2.  Le coût exact de la rotation interne lors de l'utilisation de `make_contiguous` sur un `VecDeque` n'est pas quantifié dans les sources fournies, bien qu'il soit implicite que cela nécessite une copie ou un réarrangement des données.

## 7. Considérations partagées et limites

### Gestion de la mémoire et allocation
Les deux structures partagent la même stratégie fondamentale de gestion de la capacité : elles permettent d'avoir une quantité d'espace inoccupé pour éviter les réallocations fréquentes [collections]. Cependant, il est crucial de noter que « Rust's collections never automatically shrink » [collections]. Cela signifie que si des éléments sont supprimés (via `pop` ou `remove`), la capacité allouée reste inchangée. Les opérations de retrait ne sont donc pas amortisées en termes de libération de mémoire, contrairement aux insertions.

### Limites et incertitudes
*   **Non-contiguïté native :** Le `VecDeque` n'est pas contigu par défaut. Toute tentative d'optimisation basée sur la localité des données (comme le tri in-place ou l'utilisation de pointeurs externes) échouera sans l'appel explicite à `make_contiguous` [vecdeque-struct].
*   **Performance brute :** La documentation admet que même lorsque la complexité est identique, le `Vec` est généralement plus rapide. Cette affirmation qualitative nécessite une validation empirique pour des charges de travail spécifiques [collections].
*   **Allocation dynamique :** Les deux structures allouent dynamiquement sur le tas via l'allocateur global (Global allocator) [vec-index]. La taille de la capacité ne doit pas être considérée comme minimale car l'allocateur peut fournir plus d'espace que demandé lors des appels `reserve_exact` ou `push` [vecdeque-struct].

## 8. Conclusion

L'analyse comparative de `Vec` et `VecDeque` révèle un compromis classique entre contiguïté mémoire et polyvalence d'accès. Le `Vec` reste la structure préférée pour les piles, les listes séquentielles où l'on n'écrit qu'à la fin, et les structures nécessitant une vue mémoire contigue sans overhead de rotation. Il offre des performances légèrement supérieures aux opérations de base en raison de son architecture plus simple [collections]. Le `VecDeque`, quant à lui, est indispensable pour les files d'attente (queues) et les structures nécessitant une insertion ou suppression efficace au début et à la fin. Bien que ses performances théoriques soient équivalentes aux extrémités, son implémentation en tampon annulaire le rend légèrement moins performant que le `Vec` dans des scénarios où l'on n'utilise pas ses capacités spécifiques de double extrémité [vec_deque].

**Niveau de confiance :** Élevé. Les conclusions sont directement dérivées des spécifications officielles de la bibliothèque standard de Rust, publiées et validées pour la version actuelle (2026). Aucune hypothèse externe n'a été faite concernant les implémentations d'allocateurs tiers ou les architectures matérielles spécifiques non mentionnées dans les sources.

## Sources et méthodologie

Les données présentées dans ce rapport proviennent exclusivement des documents officiels de la bibliothèque standard de Rust consultés le 19 octobre 2026 :
*   **Collections Overview :** [https://doc.rust-lang.org/std/collections/index.html](https://doc.rust-lang.org/std/collections/index.html) - Source principale pour les comparaisons de performance et les cas d'usage généraux.
*   **VecDeque Structure :** [https://doc.rust-lang.org/std/collections/struct.VecDeque.html](https://doc.rust-lang.org/std/collections/struct.VecDeque.html) - Source pour les détails API (`make_contiguous`, `capacity`, etc.) et l'architecture ring buffer.
*   **VecDeque Module :** [https://doc.rust-lang.org/std/collections/vec_deque/index.html](https://doc.rust-lang.org/std/collections/vec_deque/index.html) - Source pour la confirmation des complexités O(1) amorties aux deux extrémités.
*   **Vec Structure :** [https://doc.rust-lang.org/std/vec/index.html](https://doc.rust-lang.org/std/vec/index.html) - Source pour l'architecture contigue et les opérations de fin.

Ces sources ont été sélectionnées car elles constituent la documentation canonique des types en question, garantissant que les affirmations sur les complexités temporelles et les garanties de sécurité mémoire sont exactes par rapport à l'implémentation officielle.

## Verdicts "Best For"

*   **Utiliser `Vec` si :** Vous construisez une pile (LIFO), stockez des données pour un traitement ultérieur sans accès fréquent au début, ou avez besoin d'une tranchabilité de mémoire garantie (slice) sans appel de méthode supplémentaire. C'est le choix par défaut pour la plupart des collections séquentielles simples.
*   **Utiliser `VecDeque` si :** Vous implémentez une file (FIFO), effectuez un mélange fréquent d'éléments au début et à la fin, ou avez besoin d'une complexité O(1) garantie pour les insertions/suppressions aux deux extrémités.