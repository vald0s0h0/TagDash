# Mises à jour de TagDash

Comment publier une version de TagDash pour que les apps déjà installées se
mettent à jour **toutes seules** au prochain lancement.

> **À retenir en 1 phrase :**
> **Commit** = sauvegarde sur ton PC · **Sync** = envoi sur GitHub · **Tag** = publie la version (c'est le tag qui lance le build et crée la release).
>
> Un commit/push normal ne publie **rien**. Il faut poser un **tag** (`v0.1.0`, `v0.1.1`, …).

---

## Partie 1 — Configuration unique (à faire **une seule fois**)

### 1.0 — Le dépôt doit être **public** ⚠️

La mise à jour auto interroge `…/releases/latest/download/latest.json` **sans
authentification**. Sur un dépôt **privé**, GitHub renvoie **404** → l'app affiche
« *Could not fetch a valid release JSON* ». Le dépôt **doit donc être public**
(les installeurs sont publics, mais ton code aussi). Tes secrets restent protégés :
clé privée de signature *gitignorée*, clés API hors du repo, secrets dans *GitHub
Secrets*.
→ GitHub → **Settings → General** → tout en bas **Danger Zone** → **Change
visibility → Make public**.

### 1.A — Ajouter les 2 secrets sur GitHub (dans le navigateur)

Sans ça, le build échoue au moment de signer la mise à jour.

1. Va sur **https://github.com/vald0s0h0/TagDash**
2. Onglet **Settings** (en haut) → menu de gauche **Secrets and variables** → **Actions**
3. Bouton vert **New repository secret**, et crée ces **deux** secrets :

   | Name (nom du secret) | Secret (valeur à coller) |
   |---|---|
   | `TAURI_SIGNING_PRIVATE_KEY` | **tout le contenu** du fichier `~\.tauri\tagdash.key` (voir ci-dessous) |
   | `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | le mot de passe choisi à la création de la clé (laisse **vide** si tu n'en as pas mis) |

   👉 Pour récupérer le contenu de la clé privée : dans VS Code, ouvre le fichier
   `~/.tauri/tagdash.key` (à la racine du projet, dossier nommé `~`), **sélectionne
   tout** (clic dans le fichier puis `Ctrl+A`), **copie** (`Ctrl+C`), et **colle** dans
   le champ « Secret ».

   ⚠️ Ne mets **jamais** cette clé privée ailleurs que dans ce secret. Elle est déjà
   exclue de Git (`.gitignore`).

### 1.B — Activer « push des tags au Sync » dans VS Code (100 % souris)

Pour que le bouton **Sync** envoie aussi les tags (et donc déclenche la release).

1. En bas à gauche de VS Code, clique sur la **roue dentée ⚙️** → **Settings**
2. Dans la barre de recherche en haut, tape : `follow tags`
3. Coche la case **« Git: Follow Tags When Sync »**
4. C'est tout, le réglage est gardé.

---

## Partie 2 — Publier une mise à jour (à chaque fois)

### Étape 0 — Changer le numéro de version

⚠️ **Sauf pour la toute première release** (qui reste en `0.1.0`), il faut augmenter
la version, sinon les apps installées ne verront pas la mise à jour.

1. Ouvre le fichier **`src-tauri/tauri.conf.json`**
2. Tout en haut, change la ligne `"version": "0.1.0"` → par exemple `"version": "0.1.1"`
3. **Enregistre** (`Ctrl+S`)

> Règle des numéros : `0.1.0` → `0.1.1` → `0.1.2` … (incrémente le dernier chiffre
> pour un petit correctif). La version installée ne se met à jour que si la version
> publiée est **strictement plus grande**.

### Étape 1 — Commit (clics)

1. Clique sur l'icône **Source Control** dans la barre de gauche
   (l'icône qui ressemble à un embranchement •—<).
2. En haut, écris un **message** (ex. `release 0.1.1`).
3. Clique sur le bouton bleu **✓ Commit**.
   - S'il demande *« Voulez-vous indexer tous les changements ? »*, clique **Yes**
     (ou **Always**).

### Étape 2 — Créer le tag (clics)

1. Toujours dans **Source Control**, clique sur les **« … »** en haut à droite du
   panneau (*Views and More Actions*).
2. Survole **Tags** → clique **Create Tag**.
3. Une petite zone de saisie apparaît en haut au centre :
   - **Nom du tag** : tape `v0.1.1` puis valide (`Entrée`).
     ⚠️ Le `v` est obligatoire, et le numéro doit **correspondre** à la version de
     l'étape 0 (`0.1.1` → `v0.1.1`).
   - **Message du tag** : retape `v0.1.1` puis valide (`Entrée`).
     *(Important : mettre un message rend le tag « complet » pour qu'il soit poussé.)*

### Étape 3 — Sync (clics) → ça publie 🚀

1. Clique sur le bouton bleu **Sync Changes** (les deux flèches en rond), soit dans le
   panneau Source Control, soit en bas à gauche dans la barre d'état.
2. Grâce au réglage de la partie 1.B, ce clic envoie **le commit ET le tag** sur GitHub.
3. Le tag déclenche automatiquement le build.

### Étape 4 — Vérifier (clics, dans le navigateur)

1. Va sur **https://github.com/vald0s0h0/TagDash** → onglet **Actions**.
2. Tu vois le workflow **« Release »** en cours (rond jaune qui tourne).
3. Quand il devient **vert ✅** (≈ 5–15 min), la version est publiée :
   onglet **Code** → encart **Releases** à droite → ta version `v0.1.1` avec
   l'installeur et le fichier `latest.json`.

À partir de là, **toute app TagDash déjà installée** verra la mise à jour à son
prochain lancement (premier élément du *Startup Pipeline*), la téléchargera,
l'installera et **redémarrera automatiquement**.

---

## Partie 3 — Récupérer les installeurs pour les testeurs

> ⚠️ Tu es sous **Windows** : tu **ne peux pas** fabriquer un `.dmg` (macOS) sur ton
> PC. C'est le runner **macOS** de GitHub (configuré dans le workflow) qui le
> construit pendant la release. Donc : pas besoin de Mac, mais il faut **passer par
> une release** (Partie 2).

Quand le build est **vert ✅** (onglet Actions), les installeurs sont attachés à la
release :

1. Va sur **https://github.com/vald0s0h0/TagDash** → onglet **Code**.
2. Encart **Releases** à droite → clique sur la version (ex. `v0.1.1`).
3. Déplie **Assets** : tu y trouves les fichiers à envoyer aux testeurs —
   - **macOS Apple Silicon** (Mac M1/M2/M3…) : `TagDash_x.y.z_aarch64.dmg`
   - **macOS Intel** (anciens Mac) : `TagDash_x.y.z_x64.dmg`
   - **Windows** : `TagDash_x.y.z_x64-setup.exe` et/ou `TagDash_x.y.z_x64_en-US.msi`
   - *(les fichiers `.sig`, `.app.tar.gz` et `latest.json` servent à la mise à jour
     automatique — pas besoin de les envoyer.)*

   > Depuis v0.1.3 le `.dmg` **universel** unique est remplacé par **deux** `.dmg`
   > (un par puce). En cas de doute, sur le Mac : menu  → *À propos de ce Mac* →
   > si « Puce Apple », prends `aarch64` ; si « Processeur Intel », prends `x64`.

   > ⚠️ **Depuis 2026, GitHub ne construit plus le `.dmg` Intel `x64`** (les runners
   > Intel ont été retirés). La release GitHub ne contient donc **que** Windows +
   > **macOS Apple Silicon** (`aarch64`). Le `.dmg` **Intel** (`x64`) se fabrique
   > **à la main sur un Mac Apple Silicon** → voir la **Partie 4** ci-dessous. Tant
   > que tu ne le fais pas, il n'y a simplement pas de version Intel pour ce tag.
4. Clique sur le bon `.dmg` pour le télécharger, puis envoie-le à tes testeurs (mail,
   lien, WeTransfer…).

### Côté testeur macOS — ouvrir une app non signée Apple

L'app n'est **pas signée/notarisée Apple** (ça demande un compte Apple Developer
payant). Au 1er lancement, macOS la bloque. À dire aux testeurs **une fois** :

1. Ouvrir le `.dmg`, glisser **TagDash** dans **Applications**.
2. Si « *TagDash est endommagée / impossible à ouvrir* » :
   **Réglages Système → Confidentialité et sécurité** → tout en bas, bouton
   **« Ouvrir quand même »**.
   *(Alternative : clic droit sur l'app → **Ouvrir** → **Ouvrir**.)*

> Une fois installée, la **mise à jour automatique** fonctionne ensuite toute seule
> (vérifiée par ta clé de signature, indépendante d'Apple).

---

## Partie 4 — Fabriquer le `.dmg` macOS **Intel** (sur ton Mac Apple Silicon)

> ✨ **Le workflow GitHub tente désormais de fabriquer l'Intel automatiquement**
> (job « Intel », **expérimental** : il construit le x86_64 via Rosetta directement
> sur le runner Apple Silicon, avec le **même** script que ci-dessous).
> → Va dans **Actions** : si le job **Intel** est **vert ✅** et que le fichier
> `TagDash_x.y.z_x64.dmg` apparaît dans la release, **tu n'as rien à faire**.
> → S'il est **rouge ❌** (montage non testé, il peut demander 1–2 réglages), suis la
> méthode **manuelle** ci-dessous : c'est exactement le même script, lancé sur ton Mac.

Le script `scripts/build-macos-intel.sh` compile une version **x86_64 native** via
**Rosetta** (la traduction Intel d'Apple). C'est ce qui évite les échecs de
« cross-compilation » qui bloquaient l'ancien runner Intel. Tu peux le lancer
toi-même sur un Mac **Apple Silicon** (M1/M2/M3…).

> Cette partie se fait **dans le Terminal** du Mac (app **Terminal**), pas en clics.
> Copie/colle chaque commande puis `Entrée`.

### 4.A — Préparation (à faire **une seule fois**, sur le Mac)

1. **Rosetta** (le traducteur Intel d'Apple) :
   ```
   softwareupdate --install-rosetta --agree-to-license
   ```
2. **Homebrew version Intel** (s'installe dans `/usr/local`, à côté de celui Apple
   Silicon, sans le casser) :
   ```
   arch -x86_64 /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
   ```
3. **cmake + llvm** version Intel (nécessaires à la compilation de whisper.cpp) :
   ```
   arch -x86_64 /usr/local/bin/brew install cmake llvm
   ```
4. **Rust** : si tu n'as pas encore `rustup`, installe-le depuis **https://rustup.rs**
   (la commande à coller est affichée sur le site). Puis ajoute la **toolchain Intel** :
   ```
   rustup toolchain install stable-x86_64-apple-darwin
   ```
5. **Node 20+** : installe-le depuis **https://nodejs.org** (le `.pkg` officiel est
   **universel**, il marche sous Rosetta). ⚠️ N'utilise **pas** `brew install node`
   du Homebrew Apple Silicon : il est arm64-only et plante sous Rosetta (« *Bad CPU
   type* »).
6. **Outils de publication** (pour que les Mac Intel reçoivent la mise à jour auto) :
   ```
   arch -x86_64 /usr/local/bin/brew install gh jq
   gh auth login
   ```
   *(`gh` = GitHub en ligne de commande. À `gh auth login`, choisis* **GitHub.com**
   *→* **HTTPS** *→* **Login with a web browser** *.)*
7. **La clé de signature** : c'est la **même** que le secret GitHub (Partie 1.A).
   Copie le contenu du fichier `~/.tauri/tagdash.key` **depuis ton PC Windows** dans
   un fichier sur le Mac, par ex. `~/tagdash.key`.
   > Range-la dans ton dossier perso (`~`), **jamais** dans le projet (risque de la
   > committer). Transfère-la par un moyen sûr (clé USB, gestionnaire de mots de
   > passe…), pas par mail/chat en clair.
8. **Récupère le projet** sur le Mac :
   ```
   git clone https://github.com/vald0s0h0/TagDash.git
   ```

### 4.B — À chaque release (après le ✅ GitHub de Windows + Apple Silicon)

> À faire **après** la Partie 2 : le tag et la release doivent déjà exister sur
> GitHub (c'est sur cette release qu'on ajoute le `.dmg` Intel).

1. Place-toi sur la **version publiée** (remplace `v0.1.6` par ton tag) :
   ```
   cd TagDash
   git fetch --tags
   git checkout v0.1.6
   ```
   *(Se placer sur le tag garantit que le `.dmg` Intel a* **exactement** *la même
   version que la release.)*
2. Lance la fabrication **et** la publication :
   ```
   TAURI_SIGNING_PRIVATE_KEY="$(cat ~/tagdash.key)" \
   TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" \
   ./scripts/build-macos-intel.sh --publish v0.1.6
   ```
   - Si tu as mis un **mot de passe** à la clé (Partie 1.A), remplace `""` par ce
     mot de passe.
   - Le **premier** build est **long** (compilation de whisper.cpp, plusieurs
     minutes) ; les suivants sont plus rapides (cache).
3. À la fin, le script :
   - **téléverse** le `.dmg` Intel + les fichiers d'auto-update sur la release du tag ;
   - **ajoute** l'entrée `darwin-x86_64` dans `latest.json` → les Mac **Intel** déjà
     installés se mettront à jour **tout seuls** au prochain lancement.

> **Variante sans auto-update** (juste un `.dmg` à donner à un testeur Intel) : lance
> le script **sans** `--publish` :
> ```
> ./scripts/build-macos-intel.sh
> ```
> Le `.dmg` apparaît dans `src-tauri/target/x86_64-apple-darwin/release/bundle/dmg/`.
> Uploade-le à la main : onglet **Code** → **Releases** → ta version → **Edit** (crayon)
> → glisse le fichier dans **Assets**.

### En cas de souci (Partie 4)

| Symptôme | Solution |
|---|---|
| `x86_64 Rust toolchain missing` | `rustup toolchain install stable-x86_64-apple-darwin` (étape 4.A.4) |
| `cmake` / `libclang` introuvable pendant le build | refais 4.A.3 : `arch -x86_64 /usr/local/bin/brew install cmake llvm` |
| `gh` / `jq` introuvable (avec `--publish`) | refais 4.A.6 |
| Aucun fichier `.sig` produit | la clé n'est pas lue : vérifie `~/tagdash.key` et la commande `TAURI_SIGNING_PRIVATE_KEY="$(cat ~/tagdash.key)"` |
| `Bad CPU type in executable` | Rosetta manque (refais 4.A.1) ; le script se relance pourtant tout seul sous Rosetta |

---

## Résumé express (les 5 clics à chaque release)

1. `tauri.conf.json` → monter la version → `Ctrl+S`
2. Source Control → message → **Commit**
3. **…** → Tags → **Create Tag** → `v0.1.1` (+ message `v0.1.1`)
4. **Sync Changes**
5. GitHub → **Actions** → attendre le ✅
6. *(Mac Intel uniquement)* sur ton Mac Apple Silicon → **Partie 4.B** :
   `git checkout v0.1.1` puis `./scripts/build-macos-intel.sh --publish v0.1.1`

---

## En cas de souci

| Symptôme | Cause probable | Solution |
|---|---|---|
| Le build (Actions) passe au **rouge ❌** sur la signature | Secrets manquants ou faux | Re-vérifie la **partie 1.A** (les 2 secrets) |
| Aucune release n'apparaît | Le **tag** n'est pas parti | GitHub → onglet **Code** → **Tags** : ton tag y est-il ? Sinon refais 1.B puis l'étape 3, ou demande-moi |
| L'app ne se met pas à jour | Version pas augmentée, ou release en *draft/prerelease* | La version publiée doit être **plus grande** que l'installée ; la release doit être **publiée** (pas brouillon) |
| Le tag a un mauvais numéro | Faute de frappe | Crée un nouveau tag corrigé (ex. `v0.1.2`) et re-Sync |

> Tant que tu ne crées pas de **tag**, tu peux committer / Sync autant que tu veux
> sans jamais publier de mise à jour. La release n'arrive **que** sur un tag `v…`.
