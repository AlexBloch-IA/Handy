# Transcription de fichiers audio (mode "long form")

**Date :** 2026-07-29
**Branche :** `feature/file-transcription`
**Statut :** design validé, prêt pour le plan d'implémentation

## Problème

Handy transcrit uniquement le micro, en dictée courte : on appuie sur un raccourci, on parle quelques secondes, le texte est collé dans l'app active.

Il n'existe aucun moyen, depuis l'interface, de transcrire un fichier audio déjà enregistré — typiquement un enregistrement de call de 30 à 60 minutes. Le seul chemin existant est le flag CLI `--transcribe-file`, qui n'accepte que du WAV 16 kHz mono 16-bit et n'affiche aucune progression.

## Objectif

Ajouter un onglet **Fichiers** permettant de déposer des enregistrements audio et d'obtenir leur transcription, en réutilisant **exactement** le moteur existant : même modèle, mêmes réglages (langue, traduction, custom words), même qualité que la dictée.

## Non-objectifs

Explicitement hors périmètre, pour garder la feature alignée sur la philosophie de Handy :

- Pas de LLM, pas de résumé, pas d'action items
- Pas de diarisation (identification des locuteurs)
- Pas de timestamps ni de sous-titres (SRT/VTT)
- Pas de traitement parallèle de plusieurs fichiers
- Pas de dépendance externe à installer par l'utilisateur (pas de ffmpeg)

## Architecture

### Le point d'ancrage

Le flux micro actuel est :

```
micro → AudioRecorder → VAD → resample 16 kHz → TranscriptionManager::transcribe() → collage
```

Le flux fichier rejoint ce tuyau au même endroit :

```
fichier → décodage → resample 16 kHz → découpage VAD → TranscriptionManager::transcribe() ×N → .txt
```

`TranscriptionManager` n'est pas modifié. Sa méthode publique est déjà exactement ce qu'il faut :

```rust
// src-tauri/src/managers/transcription.rs:1112
pub fn transcribe(&self, audio: Vec<f32>) -> Result<String>
```

Elle lit les réglages persistés à chaque appel (langue, traduction, custom words, filtrage de sortie). Un bloc issu d'un fichier est donc traité rigoureusement comme un bloc issu du micro. C'est la garantie centrale de ce design : aucune divergence de qualité possible entre dictée et fichier, parce que c'est le même code.

### Backend — `managers/file_transcription.rs` (nouveau)

**1. Décodage.** Nouvelle dépendance `symphonia` (Rust pur, aucun binaire externe), features `mp3, aac, isomp4, alac, flac, vorbis, pcm`. Couvre les formats réellement produits par les outils de call : `.m4a` (Zoom, Teams, dictaphone iPhone), `.mp4`, `.mp3`, `.wav`, `.flac`, `.ogg`.

Sortie : PCM `f32`, downmixé en mono par moyenne des canaux.

**2. Resampling.** `rubato` (déjà dans `Cargo.toml`, déjà utilisé par le chemin micro) : fréquence source → 16 kHz. Court-circuité si la source est déjà à 16 kHz.

**3. Découpage.** `SileroVad` (déjà présent, modèle déjà bundlé dans `resources/models/silero_vad_v4.onnx`) parcourt le signal par frames de 30 ms et repère les silences.

Règles de coupe :

- Cible : blocs de ~120 s
- Coupe autorisée uniquement sur un silence d'au moins 500 ms, au plus près de la cible
- Si aucun silence n'est trouvé avant 180 s, coupe forcée (protection contre un flux continu sans pause)
- Les blocs sont contigus : aucun échantillon n'est perdu entre deux blocs

Le découpage est une **fonction pure** `chunk_by_silence(samples: &[f32], vad: &mut dyn VoiceActivityDetector) -> Vec<Range<usize>>`, donc testable unitairement sans fichier ni modèle de transcription.

Ce découpage a deux raisons d'être, dans cet ordre d'importance :

1. Rendre la progression réelle. Un appel unique sur 60 minutes d'audio ne remonte aucune information pendant 20 minutes.
2. Permettre l'annulation à granularité fine (entre deux blocs).

**4. Exécution.** Un worker thread dédié consomme une file FIFO de jobs. Pour chaque job :

- Neutralisation temporaire du `model_unload_timeout` (le modèle doit rester chargé pendant tout le job)
- Chargement du modèle sélectionné si nécessaire, via `load_model_with_device(&model_id, None)`
- Boucle sur les blocs → `tm.transcribe(bloc)`
- Concaténation des sorties, séparées par un espace, normalisation des espaces multiples

**5. Priorité micro.** Si l'utilisateur déclenche une dictée pendant qu'un job tourne, la dictée gagne : le worker termine le bloc en cours, puis attend que le `TranscriptionManager` se libère avant d'enchaîner. Sans cela, une transcription longue en arrière-plan rendrait le raccourci de dictée inutilisable pendant 20 minutes.

**6. Événements.** Événement Tauri `file-transcription-progress` émis après chaque bloc :

```rust
struct FileTranscriptionProgress {
    job_id: String,
    chunks_done: usize,
    chunks_total: usize,
    status: JobStatus,       // Queued | Decoding | Transcribing | Done | Failed | Cancelled
    error: Option<String>,
}
```

Le décodage d'un fichier d'une heure prend quelques secondes : il a son propre statut (`Decoding`) pour que l'UI ne paraisse pas figée avant que `chunks_total` ne soit connu.

**7. Sortie.** Écriture du transcript en `.txt` UTF-8 à côté du fichier source (`call.m4a` → `call.txt`). Si le fichier existe déjà, suffixage `call-2.txt` — jamais d'écrasement silencieux. Si le dossier source n'est pas accessible en écriture, repli sur `~/Documents` et remontée du chemin réel à l'UI.

### Backend — `commands/file_transcription.rs` (nouveau)

Trois commandes Tauri, exposées via `tauri-specta` (donc typées automatiquement dans `bindings.ts`) :

| Commande | Rôle |
| --- | --- |
| `enqueue_file_transcriptions(paths: Vec<PathBuf>) -> Result<Vec<Job>>` | Valide les extensions, crée les jobs, les met en file |
| `cancel_file_transcription(job_id: String)` | Annule un job (en attente ou en cours) |
| `list_file_transcription_jobs() -> Vec<Job>` | État courant, pour resynchroniser l'UI au montage |

Les jobs vivent en mémoire uniquement. Ils ne survivent pas à un redémarrage de l'app — le `.txt` sur disque est le seul artefact durable. C'est un choix délibéré : pas de migration SQLite, pas de reprise partielle à gérer.

### Frontend — onglet « Fichiers »

Nouvelle entrée dans `Sidebar.tsx`, placée sous Historique. Composant `src/components/settings/files/FilesSettings.tsx` avec trois zones, construites exclusivement sur les composants existants (`SettingsGroup`, `ProgressBar`, `Button`) pour rester visuellement indistinguable du reste de l'app :

1. **Zone de dépôt** — drag & drop via l'événement Tauri `tauri://drag-drop`, plus un bouton « Parcourir » ouvrant le sélecteur natif. Les fichiers d'extension non supportée sont rejetés avec un message explicite, sans être mis en file.
2. **File d'attente** — une ligne par fichier : nom, durée, `ProgressBar`, statut, bouton d'annulation. Traitement strictement séquentiel.
3. **Résultat** — le transcript du job sélectionné en lecture seule, bouton « Copier », bouton « Révéler dans le Finder » (`tauri-plugin-opener`, déjà présent).

Un avertissement s'affiche si aucun modèle n'est chargé, avec un lien vers l'onglet Modèles.

### i18n

Toutes les chaînes passent par i18next — l'ESLint du projet interdit les chaînes en dur dans le JSX. Ajout des clés dans `en/translation.json` (source) et `fr/translation.json`. Les 22 autres langues retombent sur l'anglais tant qu'elles ne sont pas traduites, ce qui est le comportement normal du projet.

## Gestion des erreurs

| Cas | Comportement |
| --- | --- |
| Format non supporté / fichier corrompu | Job marqué `Failed` avec le message du décodeur ; la file continue |
| Aucun modèle installé ou chargement en échec | Job `Failed`, message pointant vers l'onglet Modèles |
| Fichier sans parole (silence intégral) | Job `Done`, transcript vide, mention explicite dans l'UI |
| Échec d'écriture du `.txt` | Transcript conservé en mémoire et affiché ; l'erreur d'écriture est signalée séparément — le travail de transcription n'est jamais perdu à cause du disque |
| Annulation | Le bloc en cours se termine (l'inférence n'est pas interruptible), puis le job s'arrête ; aucun `.txt` écrit |
| Fermeture de l'app pendant un job | Le job est perdu, sans `.txt` partiel |

## Tests

**Unitaires (Rust) :**

- `chunk_by_silence` — signal synthétique alternant parole/silence : vérifie que les coupes tombent dans les silences, que les blocs sont contigus et couvrent tout le signal, et que la coupe forcée se déclenche à 180 s sur un signal continu
- Décodage + resampling — fixture WAV courte : vérifie la fréquence, le canal unique et la durée en sortie
- Nommage de sortie — vérifie le suffixage `-2` quand le `.txt` existe déjà

**Manuel (end-to-end), avec Whisper Large v3 chargé :**

- Un `.m4a` de call réel d'environ 45 min → progression fluide, transcript complet et cohérent
- Un `.mp3` et un `.wav` → mêmes résultats
- Dictée au raccourci pendant un job → la dictée passe, le job reprend ensuite
- Annulation en cours de job → arrêt propre, aucun `.txt`

## Fichiers touchés

**Nouveaux :**

- `src-tauri/src/managers/file_transcription.rs`
- `src-tauri/src/commands/file_transcription.rs`
- `src/components/settings/files/FilesSettings.tsx`
- `src/components/settings/files/FileDropZone.tsx`
- `src/components/settings/files/FileJobRow.tsx`
- `src/components/settings/files/index.ts`
- `src/stores/fileTranscriptionStore.ts`

**Modifiés :**

- `src-tauri/Cargo.toml` — ajout de `symphonia`
- `src-tauri/src/managers/mod.rs`, `src-tauri/src/commands/mod.rs` — déclarations
- `src-tauri/src/lib.rs` — enregistrement des commandes et du worker
- `src/components/Sidebar.tsx` — entrée de navigation
- `src/i18n/locales/en/translation.json`, `src/i18n/locales/fr/translation.json`

## Risques connus

- **Poids de `symphonia`** — ajoute des dépendances de décodage au binaire. Acceptable : c'est la seule alternative crédible à une dépendance ffmpeg externe.
- **AAC/ALAC dans `symphonia`** — le support AAC est moins mature que celui de MP3/FLAC. À valider tôt sur un vrai `.m4a` de call ; en cas d'échec, repli macOS sur `afconvert` (binaire système, présent partout).
- **Divergence avec l'upstream** — cette branche s'écarte du dépôt officiel, qui est par ailleurs en feature freeze. La feature est volontairement confinée à des fichiers nouveaux, avec un minimum de lignes touchées dans l'existant, pour que le rebase sur une future release reste trivial.
