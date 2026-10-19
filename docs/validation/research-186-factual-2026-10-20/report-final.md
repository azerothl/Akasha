# Comparaison de `Vec` et `VecDeque` en Rust

> A contiguous growable array type with heap-allocated contents, written Vec\<T\>.
> Vectors have O(1) indexing, amortized O(1) push (to the end) and O(1) pop (from the end).

[Source](<https://doc.rust-lang.org/std/vec/index.html>)

> Throughout the documentation, we will adhere to the following conventions for operation notation:
> The collection’s size is denoted by n.
> If a second collection is involved, its size is denoted by m.
> Item indices are denoted by i.
> Operations which have an amortized cost are suffixed with a \*.
> Operations with an expected cost are suffixed with a ~.
> Calling operations that add to a collection will occasionally require a collection to be resized - an extra operation that takes O(n) time.
> Amortized costs are calculated to account for the time cost of such resize operations over a sufficiently large series of operations. An individual operation may be slower or faster due to the sporadic nature of collection resizing, however the average cost per operation will approach the amortized cost.
> Rust’s collections never automatically shrink, so removal operations aren’t amortized.
> §Cost of Collection Operations
> | | get(i) | insert(i) | remove(i) | append(Vec(m)) | split\_off(i) | range | append
> |---|---|---|---|---|---|---|---|
> | Vec | O(1) | O(n-i)\* | O(n-i) | O(m)\* | O(n-i) | N/A | N/A
> | VecDeque | O(1) | O(min(i, n-i))\* | O(min(i, n-i)) | O(m)\* | O(min(i, n-i)) | N/A | N/A

[Source](<https://doc.rust-lang.org/std/collections/index.html>)

> A double-ended queue (deque) implemented with a growable ring buffer.
> This queue has O(1) amortized inserts and removals from both ends of the container. It also has O(1) indexing like a vector. The contained elements are not required to be copyable, and the queue will be sendable if the contained type is sendable.

[Source](<https://doc.rust-lang.org/std/collections/vec_deque/index.html>)

L'API du `VecDeque` expose des méthodes spécifiques pour gérer sa structure interne, comme `as_slices()` qui retourne une paire de références vers les parties front et arrière du tampon [VecDeque](https://doc.rust-lang.org/std/collections/struct.VecDeque.html). La méthode `range` permet d'itérer sur des sous-séquences sans copier les données [VecDeque](https://doc.rust-lang.org/std/collections/struct.VecDeque.html).

## Limites

Synthèse limitée aux passages lus. Les citations originales préservent leurs qualifications ; ce contrôle automatique ne remplace pas une vérification indépendante.