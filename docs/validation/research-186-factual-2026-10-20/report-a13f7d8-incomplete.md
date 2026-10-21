## Organisation mémoire et accès par indice

> A contiguous growable array type with heap-allocated contents, written Vec\<T\>.
> Vectors have O(1) indexing, amortized O(1) push (to the end) and O(1) pop (from the end).

[Source](<https://doc.rust-lang.org/std/vec/index.html>)

> A double-ended queue (deque) implemented with a growable ring buffer.
> This queue has O(1) amortized inserts and removals from both ends of the container. It also has O(1) indexing like a vector. The contained elements are not required to be copyable, and the queue will be sendable if the contained type is sendable.

[Source](<https://doc.rust-lang.org/std/collections/vec_deque/index.html>)

## Ajout et retrait aux extrémités

## Usages et limites

Les documents indiquent que pour la plupart des cas d'utilisation génériques de stockage et de traitement de données, il est recommandé d'utiliser `Vec` ou `HashMap`. Ces deux collections couvrent la majorité des besoins. Les autres collections, dont `VecDeque`, ont des cas d'usage spécifiques où elles sont le choix optimal, mais ces cas restent marginaux par rapport à l'efficacité générale de `Vec` [std/collections/index.html](https://doc.rust-lang.org/std/collections/index.html).

## Limites

Synthèse limitée aux passages lus. Les citations originales préservent leurs qualifications ; ce contrôle automatique ne remplace pas une vérification indépendante.