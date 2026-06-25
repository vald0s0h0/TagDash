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

## Résumé express (les 5 clics à chaque release)

1. `tauri.conf.json` → monter la version → `Ctrl+S`
2. Source Control → message → **Commit**
3. **…** → Tags → **Create Tag** → `v0.1.1` (+ message `v0.1.1`)
4. **Sync Changes**
5. GitHub → **Actions** → attendre le ✅

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
