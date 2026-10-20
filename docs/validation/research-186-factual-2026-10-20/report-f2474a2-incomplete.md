## Organisation mémoire et accès par indice

> A contiguous growable array type with heap-allocated contents, written Vec\<T\>.
> Vectors have O(1) indexing, amortized O(1) push (to the end) and O(1) pop (from the end).

[Source](<https://doc.rust-lang.org/std/vec/index.html>)

> A double-ended queue (deque) implemented with a growable ring buffer.
> This queue has O(1) amortized inserts and removals from both ends of the container. It also has O(1) indexing like a vector. The contained elements are not required to be copyable, and the queue will be sendable if the contained type is sendable.

[Source](<https://doc.rust-lang.org/std/collections/vec_deque/index.html>)

## Ajout et retrait aux extrémités

> You want a sequence of elements in a particular order, and will only be appending to (or near) the end.
> §Use a VecDeque when:
> You want a Vec that supports efficient insertion at both ends of the sequence.
> Throughout the documentation, we will adhere to the following conventions for operation notation:
> Operations which have an amortized cost are suffixed with a \*.
> Amortized costs are calculated to account for the time cost of such resize operations over a sufficiently large series of operations. An individual operation may be slower or faster due to the sporadic nature of collection resizing, however the average cost per operation will approach the amortized cost.
> Rust’s collections never automatically shrink, so removal operations aren’t amortized.
> \| \| get(i) \| insert(i) \| remove(i) \| append(Vec(m)) \| split\_off(i) \| range \| append

[Source](<https://doc.rust-lang.org/std/collections/index.html>)

> pub fn front(&self) -\> Option\<&T\>

[Source](<https://doc.rust-lang.org/std/collections/struct.VecDeque.html>)

## Usages et limites

L'utilisation de `VecDeque` est recommandée lorsque l'on souhaite un `Vec` supportant des insertions efficaces aux deux extrémités de la séquence [std/collections/index.html]. On utilise `Vec` si l'on veut une séquence d'éléments dans un ordre particulier et que l'on n'ajoutera qu'à (ou près de) la fin. [Source](<https://doc.rust-lang.org/std/collections/index.html>)

Les limites incluent le fait que les coûts amortisés ne garantissent pas que chaque opération individuelle sera plus rapide ou plus lente en raison de la nature sporadique du redimensionnement des collections [std/collections/index.html]. De plus, pour `VecDeque`, le point de division exact dépend des détails d'implémentation et n'est pas garanti sauf si `make_contiguous` a été appelé précédemment [std/collections/struct.VecDeque.html]. [Source](<https://doc.rust-lang.org/std/vec/index.html>) [Source](<https://doc.rust-lang.org/std/collections/struct.VecDeque.html>)

## Limites

Synthèse limitée aux passages lus. Les citations originales préservent leurs qualifications ; ce contrôle automatique ne remplace pas une vérification indépendante.