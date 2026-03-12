# Runbook : construire et lancer le serveur BitNet (pour tester le provider Akasha)

Ce runbook décrit comment obtenir l’exécutable **llama-server** du projet [BitNet](https://github.com/microsoft/BitNet) (Microsoft) et lancer le serveur d’inférence sur le port 8080, afin de tester le provider **bitnet** d’Akasha.

---

## Prérequis

- **Git** (avec support des submodules)
- **CMake** ≥ 3.22
- **Clang** ≥ 18 ou **GCC** (obligatoire : BitNet ne se compile pas avec MSVC sous Windows)
  - **Linux / macOS** : installer clang (ex. `apt install clang`, `brew install llvm`) ou utiliser gcc.
  - **Windows** : installer **Clang** via Visual Studio Installer → Modifier VS 2022 → Composants individuels → cocher « C++ Clang compiler for Windows » et « MSBuild support for LLVM (clang-cl) ». Lancer le script depuis **« Developer PowerShell for VS 2022 »** pour que `clang` soit dans le PATH.
- **Python** ≥ 3.9 (requis pour générer `include/bitnet-lut-kernels.h` avant la compilation, ainsi que pour `setup_env.py` et `run_inference_server.py`)

---

## 1. Cloner le dépôt BitNet

```bash
git clone --recursive https://github.com/microsoft/BitNet.git
cd BitNet
```

Le sous-module `3rdparty/llama.cpp` doit être présent. Si le clone n’était pas récursif : `git submodule update --init --recursive`.

---

## 2. Générer les en-têtes requis puis construire (llama-server)

Le fichier **`include/bitnet-lut-kernels.h`** n’est pas dans le dépôt : il est généré par les scripts Python (voir `setup_env.py`, étape `gen_code()`). Il faut l’avoir **avant** d’exécuter CMake.

**Option A — Script officiel (recommandé)**  
Depuis la racine du dépôt BitNet, exécuter une fois `setup_env.py` avec un modèle (télécharge le modèle et génère les headers + compile) :

```bash
pip install -r requirements.txt
python setup_env.py -md models/BitNet-b1.58-2B-4T -q i2_s
```

Cela génère `include/bitnet-lut-kernels.h`, configure et compile le projet, puis prépare le modèle. L’exécutable est dans **`build/bin/llama-server`** (ou `build/bin/Release/llama-server.exe` sous Windows).

**Option B — Génération du header seule puis CMake**  
Si vous voulez seulement compiler sans télécharger le modèle tout de suite :

**Linux / macOS (x86_64) :**

```bash
python3 utils/codegen_tl2.py --model bitnet_b1_58-3B --BM 160,320,320 --BK 96,96,96 --bm 32,32,32
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build -j
```

**Linux / macOS (ARM64) :**

```bash
python3 utils/codegen_tl1.py --model bitnet_b1_58-3B --BM 160,320,320 --BK 64,128,64 --bm 32,64,32
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build -j
```

L’exécutable est produit dans **`build/bin/llama-server`**.

**Windows (Developer PowerShell for VS 2022, avec Clang installé) :**

Générer d’abord le header (comme sous Linux) :

```powershell
python utils/codegen_tl2.py --model bitnet_b1_58-3B --BM 160,320,320 --BK 96,96,96 --bm 32,32,32
```

BitNet exige Clang ou GCC ; le compilateur MSVC seul ne suffit pas. Utiliser le **toolset ClangCL** :

```powershell
cmake -S . -B build -G "Visual Studio 17 2022" -A x64 -T ClangCL
cmake --build build --config Release
```

Si vous avez **Ninja** et **clang** dans le PATH (ex. après installation de LLVM ou des composants Clang de VS) :

```powershell
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Release -DCMAKE_C_COMPILER=clang -DCMAKE_CXX_COMPILER=clang++
cmake --build build -j
```

L’exécutable est en **`build\bin\Release\llama-server.exe`** (ou parfois `build\bin\llama-server.exe`). Le script **`scripts/bitnet-build.ps1`** du dépôt Akasha configure et compile automatiquement si `clang` est disponible. Il applique des contournements pour Windows (désactivation de GGML_LLAMAFILE, flags `-mavxvnni`, correctif const dans `ggml-bitnet-mad.cpp`) pour éviter les erreurs « undeclared identifier _mm256_dpbusd_epi32 » et « cannot initialize … const int8_t * ». Le CPU doit supporter AVX-VNNI pour que l’exécutable fonctionne correctement.

En cas d’erreur avec clang, vous pouvez forcer clang-18+ (Linux/macOS) :

```bash
export CC=clang-18
export CXX=clang++-18
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build -j
```

---

## 3. Télécharger un modèle GGUF

BitNet utilise des modèles au format GGUF (quantification i2_s ou tl1). Exemple avec le modèle officiel 2B :

**Option A — Hugging Face (fichier GGUF déjà prêt) :**

```bash
# installer huggingface-cli si besoin : pip install huggingface_hub
huggingface-cli download microsoft/BitNet-b1.58-2B-4T-gguf --local-dir models/BitNet-b1.58-2B-4T
```

Le fichier GGUF à utiliser sera dans `models/BitNet-b1.58-2B-4T/` (nom du type `ggml-model-i2_s.gguf` ou similaire).

**Option B — Script BitNet (téléchargement + conversion si besoin) :**

```bash
pip install -r requirements.txt
python setup_env.py -md models/BitNet-b1.58-2B-4T -q i2_s
```

Cela remplit `models/BitNet-b1.58-2B-4T/` avec le fichier GGUF attendu.

---

## 4. Lancer le serveur

**Méthode 1 — Ligne de commande directe (Linux / macOS) :**

```bash
./build/bin/llama-server -m models/BitNet-b1.58-2B-4T/ggml-model-i2_s.gguf --host 127.0.0.1 --port 8080 -c 2048 -t 2 -cb
```

**Windows (PowerShell) :**

```powershell
.\build\bin\Release\llama-server.exe -m models/BitNet-b1.58-2B-4T/ggml-model-i2_s.gguf --host 127.0.0.1 --port 8080 -c 2048 -t 2 -cb
```

**Méthode 2 — Script Python BitNet :**

Depuis la racine du dépôt BitNet (avec le modèle déjà présent) :

```bash
python run_inference_server.py -m models/BitNet-b1.58-2B-4T/ggml-model-i2_s.gguf --port 8080
```

Le script appelle le binaire `build/bin/llama-server` (ou `build\bin\Release\llama-server.exe` sous Windows).

---

## 5. Tester avec Akasha

1. Vérifier que le serveur répond : `curl http://127.0.0.1:8080/health` ou ouvrir `http://127.0.0.1:8080` dans un navigateur (certaines versions exposent une page ou un endpoint).
2. Dans `llm_router.yaml`, configurer le provider BitNet (optionnel si défaut 8080) :

   ```yaml
   providers:
     bitnet:
       base_url: "http://127.0.0.1:8080"
   task_types:
     conversation:
       primary:
         provider: bitnet
         model: default
   ```

3. Démarrer le daemon Akasha (`akasha start`) et utiliser l’UI ou la TUI ; les requêtes conversation doivent passer par le serveur BitNet.

---

## Scripts d’aide (dans le dépôt Akasha)

Les scripts **`scripts/bitnet-build.sh`** (Linux/macOS) et **`scripts/bitnet-build.ps1`** (Windows) automatisent le clone et le build de BitNet dans un répertoire local. Voir les commentaires en tête de chaque script pour l’usage et les prérequis. Ils ne téléchargent pas le modèle ; suivre l’étape 3 ci‑dessus après le build.

---

## Dépannage

- **Erreur de compilation (chrono, etc.)** : voir les issues BitNet (ex. [Issue #78](https://github.com/microsoft/BitNet/issues/78)) ; souvent résolu en mettant à jour le sous-module `3rdparty/llama.cpp` ou en utilisant une version de clang plus récente.
- **Port 8080 déjà utilisé** : utiliser `--port 8081` (ou autre) et mettre à jour `providers.bitnet.base_url` dans `llm_router.yaml` avec le bon port.
- **Modèle introuvable** : vérifier le chemin passé à `-m` et que le fichier `.gguf` existe bien dans le dossier du modèle.

### Serveur qui se coupe tout seul (crash / exit après une requête)

Certains builds ou configurations du serveur BitNet/llama-server peuvent quitter brutalement après le premier traitement (souvent juste après « prompt done », au début de la phase de génération ou au moment d’envoyer la première réponse). **Si `llama-cli` fonctionne avec le même modèle** (même `.gguf`, même `-c`), l’inférence et le modèle sont corrects : le bug est alors **spécifique au serveur** (gestion des slots, envoi de la réponse HTTP, ou boucle de génération côté serveur), pas au noyau BitNet/llama. Causes possibles côté serveur : bug dans la boucle slot/response de llama-server, OOM spécifique au mode serveur, ou assertion lors de l’écriture de la première réponse.

**Indices dans les logs :** Si au chargement du modèle vous voyez `llm_load_vocab: GENERATION QUALITY WILL BE DEGRADED!`, `special_eos_id is not in special_eog_ids` ou de nombreux `control token ... is not marked as EOG`, le GGUF peut avoir une config tokenizer incorrecte — cela peut contribuer au crash en génération. Essayer un autre artefact de modèle (ex. autre source HuggingFace ou modèle régénéré avec `setup_env.py`) ou suivre les issues BitNet pour un correctif.

**Contournements recommandés :**

1. **Réduire le contexte** : lancer avec une fenêtre plus petite, ex. `-c 1024` ou `-c 512` au lieu de `-c 2048`, pour limiter l’usage mémoire et les cas de troncature.
2. **Éviter les prompts très longs** : si le prompt dépasse largement `n_ctx`, la troncature peut déclencher des bugs ; réduire la taille du contexte ou du prompt côté client.
3. **Relance automatique** : lancer le serveur dans une boucle pour qu’il redémarre en cas de sortie (ex. `while true; do ./build/bin/llama-server -m ... ; sleep 2; done` sous Linux/macOS ; sous Windows, script PowerShell avec une boucle `while ($true) { ... ; Start-Sleep 2 }`).
4. **Remonter le problème** : ouvrir une issue sur [microsoft/BitNet](https://github.com/microsoft/BitNet) en joignant : la version du binaire (ligne « build: … »), les options de lancement (`-c`, `-t`, etc.), le log complet du chargement du modèle (y compris les avertissements `llm_load_vocab` / `special_eos_id` / EOG) et la séquence « prompt done » puis sortie sans message. **Indiquer si `llama-cli` fonctionne avec le même modèle** : si oui, préciser que le crash est spécifique à llama-server (réponse HTTP / slots), ce qui cible le diagnostic côté serveur.

Côté Akasha, si le serveur a quitté, le provider BitNet renverra une erreur (connexion refusée / timeout) ; l’utilisateur peut relancer le serveur manuellement ou utiliser un script de relance ci‑dessus.
