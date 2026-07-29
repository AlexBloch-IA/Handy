# Transcription de fichiers audio — Plan d'implémentation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ajouter un onglet « Fichiers » à Handy qui transcrit des enregistrements audio longs (calls) en réutilisant le moteur de transcription existant, avec drag & drop, file d'attente et barre de progression.

**Architecture :** Un nouveau `FileTranscriptionManager` décode un fichier audio en PCM 16 kHz mono, le découpe en blocs de ~2 min aux silences détectés par le VAD, puis appelle `TranscriptionManager::transcribe()` — la méthode déjà utilisée par le micro — bloc par bloc sur un worker thread. La progression remonte au frontend par événement Tauri. Le résultat est écrit en `.txt` à côté du fichier source.

**Tech Stack :** Rust / Tauri 2 / `symphonia` (décodage) / `rubato` (resampling, déjà présent) / Silero VAD (déjà présent) / React + TypeScript + Zustand + Tailwind

**Spec de référence :** `docs/superpowers/specs/2026-07-29-file-transcription-design.md`

---

## Conventions du projet à respecter

Avant de commencer, l'implémenteur doit savoir :

- **Commandes de build** — toujours depuis la racine du repo :
  - Tests Rust : `cargo test --manifest-path src-tauri/Cargo.toml`
  - Dev : `CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri dev`
  - Build : `CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri build`
  - Lint front : `bun run lint`
  - Format : `bun run format`
  - Rust doit être dans le PATH : `. "$HOME/.cargo/env"`
- **i18n obligatoire** — l'ESLint du projet interdit toute chaîne littérale dans le JSX. Chaque texte passe par `t("clé")`.
- **`bindings.ts` est auto-généré** — ne jamais l'éditer à la main. Il est régénéré par `tauri dev`/`tauri build` en mode debug via `tauri-specta`.
- **Commits conventionnels** — `feat:`, `fix:`, `docs:`, `refactor:`, `chore:`.
- **Le build final sort en erreur** sur `TAURI_SIGNING_PRIVATE_KEY` après avoir produit `Handy.app`. C'est attendu et sans gravité — vérifier que `Built application at:` et `Bundling Handy.app` apparaissent avant l'erreur.

## Structure des fichiers

| Fichier | Responsabilité |
| --- | --- |
| `src-tauri/src/managers/file_transcription/mod.rs` | Types publics du job, `FileTranscriptionManager`, worker thread |
| `src-tauri/src/managers/file_transcription/decode.rs` | Fichier audio → `Vec<f32>` 16 kHz mono |
| `src-tauri/src/managers/file_transcription/chunk.rs` | Découpage en blocs aux silences (fonction pure + wrapper VAD) |
| `src-tauri/src/managers/file_transcription/output.rs` | Choix du chemin `.txt` de sortie, écriture |
| `src-tauri/src/commands/file_transcription.rs` | Commandes Tauri exposées au frontend |
| `src/stores/fileTranscriptionStore.ts` | État des jobs côté front (Zustand) + abonnement aux événements |
| `src/components/settings/files/FilesSettings.tsx` | Écran de l'onglet |
| `src/components/settings/files/FileDropZone.tsx` | Zone de dépôt + bouton Parcourir |
| `src/components/settings/files/FileJobRow.tsx` | Une ligne de la file d'attente |
| `src/components/settings/files/index.ts` | Ré-exports |

Le manager est découpé en quatre fichiers parce que décodage, découpage et nommage de sortie sont trois responsabilités indépendantes, chacune testable seule. Les mettre dans un seul fichier produirait un module de ~600 lignes difficile à faire évoluer.

---

## Task 1 : Dépendance `symphonia` et squelette du module

**Files:**
- Modify: `src-tauri/Cargo.toml`
- Create: `src-tauri/src/managers/file_transcription/mod.rs`
- Modify: `src-tauri/src/managers/mod.rs`

- [ ] **Step 1 : Ajouter la dépendance**

Dans `src-tauri/Cargo.toml`, dans la section `[dependencies]`, juste après la ligne `hound = "3.5.1"` :

```toml
# Décodage des formats de call (m4a/aac, mp4, mp3, flac, ogg, wav). Rust pur :
# évite d'imposer ffmpeg à l'utilisateur.
symphonia = { version = "0.5.4", features = [
  "mp3",
  "aac",
  "isomp4",
  "alac",
  "flac",
  "vorbis",
  "ogg",
  "wav",
  "pcm",
] }
```

- [ ] **Step 2 : Créer le module avec les types publics**

Créer `src-tauri/src/managers/file_transcription/mod.rs` :

```rust
//! Transcription de fichiers audio longs (enregistrements de call).
//!
//! Ce module ne contient aucun moteur d'inférence : il décode, découpe, puis
//! délègue chaque bloc à `TranscriptionManager::transcribe()` — exactement la
//! méthode qu'emprunte le chemin micro. Fichier et dictée ne peuvent donc pas
//! diverger en qualité.

pub mod chunk;
pub mod decode;
pub mod output;

use serde::{Deserialize, Serialize};
use specta::Type;

/// Extensions acceptées par la zone de dépôt. Tout le reste est rejeté avant
/// même la mise en file, pour que l'utilisateur ait un retour immédiat.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "m4a", "mp4", "mp3", "wav", "flac", "ogg", "oga", "aac", "caf",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Decoding,
    Transcribing,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct FileTranscriptionJob {
    pub id: String,
    /// Chemin absolu du fichier source.
    pub source_path: String,
    /// Nom de fichier seul, pour l'affichage.
    pub file_name: String,
    pub status: JobStatus,
    pub chunks_done: usize,
    /// 0 tant que le décodage n'a pas eu lieu.
    pub chunks_total: usize,
    /// Durée de l'audio en secondes, connue après décodage.
    pub duration_secs: f64,
    /// Transcript final, renseigné quand `status == Done`.
    pub transcript: Option<String>,
    /// Chemin du `.txt` écrit, si l'écriture a réussi.
    pub output_path: Option<String>,
    pub error: Option<String>,
}

impl FileTranscriptionJob {
    pub fn new(id: String, source_path: std::path::PathBuf) -> Self {
        let file_name = source_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| source_path.to_string_lossy().to_string());
        Self {
            id,
            source_path: source_path.to_string_lossy().to_string(),
            file_name,
            status: JobStatus::Queued,
            chunks_done: 0,
            chunks_total: 0,
            duration_secs: 0.0,
            transcript: None,
            output_path: None,
            error: None,
        }
    }
}

/// Événement de progression émis après chaque bloc transcrit.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct FileTranscriptionProgress {
    pub job: FileTranscriptionJob,
}
```

- [ ] **Step 3 : Déclarer le module**

Dans `src-tauri/src/managers/mod.rs`, ajouter la ligne (en respectant l'ordre alphabétique des `pub mod` existants) :

```rust
pub mod file_transcription;
```

- [ ] **Step 4 : Créer les trois sous-modules vides**

Créer `src-tauri/src/managers/file_transcription/decode.rs`, `chunk.rs` et `output.rs`, chacun contenant pour l'instant :

```rust
// Implémenté dans les tâches suivantes.
```

- [ ] **Step 5 : Vérifier que ça compile**

Run: `. "$HOME/.cargo/env" && cargo check --manifest-path src-tauri/Cargo.toml`
Expected: `Finished` sans erreur. Le téléchargement de `symphonia` ajoute ~30 s la première fois.

- [ ] **Step 6 : Commit**

```bash
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/managers/mod.rs src-tauri/src/managers/file_transcription/
git commit -m "feat: squelette du module de transcription de fichiers

symphonia plutôt qu'un appel à ffmpeg : l'utilisateur ne doit rien avoir à
installer pour transcrire un m4a."
```

---

## Task 2 : Découpage en blocs (fonction pure, TDD)

Le cœur testable de la feature. `chunk_ranges` ne connaît ni le VAD, ni ONNX, ni l'audio : elle reçoit un masque parole/silence et rend des intervalles. C'est ce qui la rend testable sans modèle ni fichier.

**Files:**
- Modify: `src-tauri/src/managers/file_transcription/chunk.rs`

- [ ] **Step 1 : Écrire le test qui échoue**

Remplacer le contenu de `src-tauri/src/managers/file_transcription/chunk.rs` par :

```rust
//! Découpage d'un signal 16 kHz en blocs transcriptibles.
//!
//! Deux raisons de découper, dans cet ordre : rendre la progression réelle
//! (un appel unique sur 60 min ne remonte rien pendant 20 min), et permettre
//! l'annulation entre deux blocs.

use std::ops::Range;

/// 30 ms à 16 kHz — la taille de frame qu'attend le VAD Silero.
pub const FRAME_SAMPLES: usize = 480;
/// Taille visée d'un bloc : 2 min.
pub const TARGET_CHUNK_SAMPLES: usize = 120 * 16_000;
/// Coupe forcée si aucun silence exploitable n'apparaît : 3 min.
pub const MAX_CHUNK_SAMPLES: usize = 180 * 16_000;
/// Un silence doit durer ~500 ms pour être une frontière de phrase crédible.
pub const MIN_SILENCE_FRAMES: usize = 17;

#[cfg(test)]
mod tests {
    use super::*;

    /// Construit un masque de frames à partir de segments (durée en frames,
    /// parole ou non).
    fn mask(segments: &[(usize, bool)]) -> Vec<bool> {
        let mut out = Vec::new();
        for (len, is_speech) in segments {
            out.extend(std::iter::repeat(*is_speech).take(*len));
        }
        out
    }

    fn frames_for_secs(secs: usize) -> usize {
        secs * 16_000 / FRAME_SAMPLES
    }

    #[test]
    fn short_signal_stays_a_single_chunk() {
        // 30 s de parole : bien en deçà de la cible de 120 s.
        let m = mask(&[(frames_for_secs(30), true)]);
        let total = m.len() * FRAME_SAMPLES;

        let ranges = chunk_ranges(&m, total);

        assert_eq!(ranges, vec![0..total]);
    }

    #[test]
    fn cuts_inside_the_silence_that_follows_the_target() {
        // 130 s de parole, 1 s de silence, 30 s de parole.
        // La coupe doit tomber dans le silence, pas au milieu d'un mot.
        let speech_a = frames_for_secs(130);
        let silence = frames_for_secs(1);
        let speech_b = frames_for_secs(30);
        let m = mask(&[(speech_a, true), (silence, false), (speech_b, true)]);
        let total = m.len() * FRAME_SAMPLES;

        let ranges = chunk_ranges(&m, total);

        assert_eq!(ranges.len(), 2, "attendu deux blocs, obtenu {:?}", ranges);
        let cut = ranges[0].end;
        let silence_start = speech_a * FRAME_SAMPLES;
        let silence_end = (speech_a + silence) * FRAME_SAMPLES;
        assert!(
            cut >= silence_start && cut <= silence_end,
            "la coupe {} doit tomber dans le silence [{}, {}]",
            cut,
            silence_start,
            silence_end
        );
    }

    #[test]
    fn ranges_are_contiguous_and_cover_everything() {
        let m = mask(&[
            (frames_for_secs(130), true),
            (frames_for_secs(1), false),
            (frames_for_secs(130), true),
            (frames_for_secs(1), false),
            (frames_for_secs(40), true),
        ]);
        let total = m.len() * FRAME_SAMPLES;

        let ranges = chunk_ranges(&m, total);

        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, total);
        for pair in ranges.windows(2) {
            assert_eq!(
                pair[0].end, pair[1].start,
                "aucun échantillon ne doit être perdu entre deux blocs"
            );
        }
    }

    #[test]
    fn forces_a_cut_when_speech_never_pauses() {
        // 200 s de parole continue : sans coupe forcée, un seul bloc de 200 s.
        let m = mask(&[(frames_for_secs(200), true)]);
        let total = m.len() * FRAME_SAMPLES;

        let ranges = chunk_ranges(&m, total);

        assert!(ranges.len() >= 2, "attendu une coupe forcée, obtenu {:?}", ranges);
        assert!(
            ranges.iter().all(|r| r.len() <= MAX_CHUNK_SAMPLES + FRAME_SAMPLES),
            "aucun bloc ne doit dépasser la limite dure"
        );
    }

    #[test]
    fn short_silences_do_not_trigger_a_cut() {
        // 130 s de parole, 200 ms de silence (respiration), 130 s de parole.
        // Le silence est trop court pour être une frontière : la coupe doit
        // attendre le suivant, pas hacher au milieu d'une phrase.
        let brief = 6; // ~180 ms
        let m = mask(&[
            (frames_for_secs(130), true),
            (brief, false),
            (frames_for_secs(20), true),
        ]);
        let total = m.len() * FRAME_SAMPLES;

        let ranges = chunk_ranges(&m, total);

        assert_eq!(ranges, vec![0..total]);
    }

    #[test]
    fn empty_input_yields_no_ranges() {
        assert!(chunk_ranges(&[], 0).is_empty());
    }
}
```

- [ ] **Step 2 : Lancer le test pour vérifier qu'il échoue**

Run: `. "$HOME/.cargo/env" && cargo test --manifest-path src-tauri/Cargo.toml file_transcription::chunk 2>&1 | tail -20`
Expected: échec de compilation, `cannot find function 'chunk_ranges' in this scope`.

- [ ] **Step 3 : Implémenter**

Insérer, dans `chunk.rs`, entre les constantes et le `mod tests` :

```rust
/// Découpe un masque parole/silence en intervalles d'échantillons contigus.
///
/// `speech[i]` décrit la frame `i` de `FRAME_SAMPLES` échantillons.
/// Les intervalles rendus sont contigus et couvrent `0..total_samples`.
pub fn chunk_ranges(speech: &[bool], total_samples: usize) -> Vec<Range<usize>> {
    if speech.is_empty() || total_samples == 0 {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < speech.len() {
        if !speech[i] {
            // Mesurer la plage de silence d'un seul tenant.
            let run_start = i;
            while i < speech.len() && !speech[i] {
                i += 1;
            }
            let run_len = i - run_start;
            // Couper au milieu du silence : on laisse une queue au bloc
            // précédent et une amorce au suivant, ce qui évite de tronquer
            // une fin de mot.
            let cut = ((run_start + i) / 2) * FRAME_SAMPLES;

            if run_len >= MIN_SILENCE_FRAMES
                && cut > start
                && cut - start >= TARGET_CHUNK_SAMPLES
            {
                ranges.push(start..cut);
                start = cut;
            }
            continue;
        }

        let pos = i * FRAME_SAMPLES;
        if pos > start && pos - start >= MAX_CHUNK_SAMPLES {
            ranges.push(start..pos);
            start = pos;
        }
        i += 1;
    }

    if start < total_samples {
        ranges.push(start..total_samples);
    }

    ranges
}
```

- [ ] **Step 4 : Lancer les tests**

Run: `. "$HOME/.cargo/env" && cargo test --manifest-path src-tauri/Cargo.toml file_transcription::chunk 2>&1 | tail -20`
Expected: `test result: ok. 6 passed; 0 failed`

- [ ] **Step 5 : Ajouter le wrapper VAD**

Ce wrapper n'est pas testé unitairement : il dépend du modèle ONNX. Sa logique se limite à produire le masque que `chunk_ranges` consomme.

Ajouter dans `chunk.rs`, avant `mod tests` :

```rust
use crate::audio_toolkit::vad::VoiceActivityDetector;
use anyhow::Result;

/// Produit le masque parole/silence en faisant tourner le VAD sur tout le
/// signal, puis délègue la décision de découpe à `chunk_ranges`.
pub fn chunk_with_vad(
    samples: &[f32],
    vad: &mut dyn VoiceActivityDetector,
) -> Result<Vec<Range<usize>>> {
    vad.reset();

    let mut speech = Vec::with_capacity(samples.len() / FRAME_SAMPLES + 1);
    for frame in samples.chunks(FRAME_SAMPLES) {
        if frame.len() < FRAME_SAMPLES {
            // Dernière frame incomplète : la traiter comme du silence évite de
            // nourrir le VAD avec une frame mal dimensionnée.
            speech.push(false);
            break;
        }
        speech.push(vad.is_voice(frame)?);
    }

    Ok(chunk_ranges(&speech, samples.len()))
}
```

- [ ] **Step 6 : Vérifier la compilation puis commit**

Run: `. "$HOME/.cargo/env" && cargo test --manifest-path src-tauri/Cargo.toml file_transcription::chunk 2>&1 | tail -5`
Expected: `test result: ok. 6 passed`

```bash
git add src-tauri/src/managers/file_transcription/chunk.rs
git commit -m "feat: découpage d'un long signal audio aux silences

Couper aux silences plutôt qu'à intervalle fixe évite de trancher au milieu
d'un mot, ce que Whisper transcrit mal des deux côtés de la coupe."
```

---

## Task 3 : Décodage audio → 16 kHz mono (TDD)

**Files:**
- Modify: `src-tauri/src/managers/file_transcription/decode.rs`

- [ ] **Step 1 : Écrire le test qui échoue**

Le test génère lui-même un WAV 44,1 kHz stéréo, donc aucun fichier binaire à committer.

Remplacer le contenu de `src-tauri/src/managers/file_transcription/decode.rs` par :

```rust
//! Décodage d'un fichier audio arbitraire vers le format qu'attend le moteur
//! de transcription : PCM f32, mono, 16 kHz.

#[cfg(test)]
mod tests {
    use super::*;

    /// Écrit un WAV 44,1 kHz stéréo 16-bit de `secs` secondes (silence) et
    /// rend son chemin dans un dossier temporaire.
    fn write_stereo_wav(dir: &std::path::Path, secs: u32) -> std::path::PathBuf {
        let path = dir.join("fixture.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..(44_100 * secs) {
            writer.write_sample(0i16).unwrap();
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();
        path
    }

    #[test]
    fn decodes_stereo_44k_to_mono_16k() {
        let dir = std::env::temp_dir().join("handy_decode_test_1");
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_stereo_wav(&dir, 2);

        let samples = decode_to_16k_mono(&path).unwrap();

        // 2 s à 16 kHz = 32 000 échantillons. Le resampler introduit un léger
        // décalage de bord, d'où la tolérance.
        let expected = 2 * 16_000;
        let delta = (samples.len() as i64 - expected as i64).abs();
        assert!(
            delta < 1_000,
            "attendu ~{} échantillons, obtenu {}",
            expected,
            samples.len()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_a_file_that_is_not_audio() {
        let dir = std::env::temp_dir().join("handy_decode_test_2");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-audio.mp3");
        std::fs::write(&path, b"ceci n'est pas un fichier audio").unwrap();

        let result = decode_to_16k_mono(&path);

        assert!(result.is_err(), "un fichier corrompu doit remonter une erreur");

        std::fs::remove_dir_all(&dir).ok();
    }
}
```

- [ ] **Step 2 : Lancer le test pour vérifier qu'il échoue**

Run: `. "$HOME/.cargo/env" && cargo test --manifest-path src-tauri/Cargo.toml file_transcription::decode 2>&1 | tail -20`
Expected: `cannot find function 'decode_to_16k_mono' in this scope`

- [ ] **Step 3 : Implémenter**

Insérer en haut de `decode.rs`, avant `mod tests` :

```rust
use crate::audio_toolkit::audio::FrameResampler;
use anyhow::{anyhow, Result};
use std::path::Path;
use std::time::Duration;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const TARGET_SAMPLE_RATE: usize = 16_000;

/// Décode un fichier audio en PCM f32 mono 16 kHz.
///
/// Les canaux sont moyennés plutôt que réduits au canal gauche : sur un
/// enregistrement de call où chaque interlocuteur atterrit sur un canal,
/// garder un seul canal ferait disparaître une des deux voix.
pub fn decode_to_16k_mono(path: &Path) -> Result<Vec<f32>> {
    let file = std::fs::File::open(path)
        .map_err(|e| anyhow!("impossible d'ouvrir {}: {}", path.display(), e))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|e| anyhow!("format audio non reconnu: {}", e))?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("aucune piste audio décodable dans le fichier"))?;
    let track_id = track.id;

    let source_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("fréquence d'échantillonnage inconnue"))? as usize;
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(1)
        .max(1);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| anyhow!("codec non supporté: {}", e))?;

    let mut mono: Vec<f32> = Vec::new();
    let mut buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Fin de flux : symphonia la signale par une erreur d'E/S.
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(e) => return Err(anyhow!("lecture du flux interrompue: {}", e)),
        };

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                let sbuf = buf.get_or_insert_with(|| {
                    SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec())
                });
                sbuf.copy_interleaved_ref(decoded);
                for frame in sbuf.samples().chunks(channels) {
                    mono.push(frame.iter().sum::<f32>() / channels as f32);
                }
            }
            // Un paquet abîmé ne doit pas condamner tout le fichier.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(anyhow!("échec du décodage: {}", e)),
        }
    }

    if mono.is_empty() {
        return Err(anyhow!("le fichier ne contient aucun échantillon audio"));
    }

    if source_rate == TARGET_SAMPLE_RATE {
        return Ok(mono);
    }

    let mut resampler = FrameResampler::new(
        source_rate,
        TARGET_SAMPLE_RATE,
        Duration::from_millis(30),
    );
    let mut out: Vec<f32> = Vec::with_capacity(mono.len() * TARGET_SAMPLE_RATE / source_rate + 1);
    resampler.push(&mono, |frame| out.extend_from_slice(frame));
    resampler.finish(|frame| out.extend_from_slice(frame));

    Ok(out)
}
```

- [ ] **Step 4 : Lancer les tests**

Run: `. "$HOME/.cargo/env" && cargo test --manifest-path src-tauri/Cargo.toml file_transcription::decode 2>&1 | tail -20`
Expected: `test result: ok. 2 passed; 0 failed`

Si `FrameResampler::new` ou `push` ne correspond pas à cette signature, lire `src-tauri/src/audio_toolkit/audio/resampler.rs:16-95` et adapter — c'est du code existant, il fait autorité sur ce plan.

- [ ] **Step 5 : Commit**

```bash
git add src-tauri/src/managers/file_transcription/decode.rs
git commit -m "feat: décodage des formats de call vers PCM 16 kHz mono

Moyenne des canaux plutôt que canal gauche : sur un call enregistré en
stéréo par interlocuteur, garder un canal supprimerait une voix."
```

---

## Task 4 : Chemin de sortie `.txt` (TDD)

**Files:**
- Modify: `src-tauri/src/managers/file_transcription/output.rs`

- [ ] **Step 1 : Écrire le test qui échoue**

Remplacer le contenu de `src-tauri/src/managers/file_transcription/output.rs` par :

```rust
//! Choix du chemin de sortie du transcript et écriture sur disque.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_the_source_name_with_a_txt_extension() {
        let dir = std::env::temp_dir().join("handy_output_test_1");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("mon-call.m4a");
        std::fs::write(&source, b"x").unwrap();

        let out = unique_output_path(&source);

        assert_eq!(out, dir.join("mon-call.txt"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn never_overwrites_an_existing_transcript() {
        let dir = std::env::temp_dir().join("handy_output_test_2");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("call.m4a");
        std::fs::write(&source, b"x").unwrap();
        std::fs::write(dir.join("call.txt"), b"transcript precedent").unwrap();

        let out = unique_output_path(&source);

        assert_eq!(out, dir.join("call-2.txt"));
        // Le fichier existant doit être intact.
        assert_eq!(
            std::fs::read_to_string(dir.join("call.txt")).unwrap(),
            "transcript precedent"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn keeps_incrementing_past_the_first_collision() {
        let dir = std::env::temp_dir().join("handy_output_test_3");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("call.m4a");
        std::fs::write(&source, b"x").unwrap();
        std::fs::write(dir.join("call.txt"), b"a").unwrap();
        std::fs::write(dir.join("call-2.txt"), b"b").unwrap();

        let out = unique_output_path(&source);

        assert_eq!(out, dir.join("call-3.txt"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
```

- [ ] **Step 2 : Lancer le test pour vérifier qu'il échoue**

Run: `. "$HOME/.cargo/env" && cargo test --manifest-path src-tauri/Cargo.toml file_transcription::output 2>&1 | tail -20`
Expected: `cannot find function 'unique_output_path' in this scope`

- [ ] **Step 3 : Implémenter**

Insérer en haut de `output.rs`, avant `mod tests` :

```rust
use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// Rend un chemin `.txt` libre à côté du fichier source.
///
/// Ne renvoie jamais un chemin déjà occupé : une transcription ne doit pas
/// pouvoir effacer la précédente.
pub fn unique_output_path(source: &Path) -> PathBuf {
    let dir = source.parent().unwrap_or_else(|| Path::new("."));
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "transcript".to_string());

    let first = dir.join(format!("{}.txt", stem));
    if !first.exists() {
        return first;
    }

    let mut n = 2u32;
    loop {
        let candidate = dir.join(format!("{}-{}.txt", stem, n));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Écrit le transcript à côté du fichier source ; si le dossier n'est pas
/// accessible en écriture (volume monté en lecture seule, dossier système),
/// se rabat sur le dossier Documents plutôt que de perdre le travail.
pub fn write_transcript(source: &Path, transcript: &str) -> Result<PathBuf> {
    let target = unique_output_path(source);
    if std::fs::write(&target, transcript).is_ok() {
        return Ok(target);
    }

    let fallback_dir = dirs_documents()
        .ok_or_else(|| anyhow!("écriture impossible et dossier Documents introuvable"))?;
    let name = target
        .file_name()
        .map(|n| n.to_os_string())
        .ok_or_else(|| anyhow!("nom de fichier de sortie invalide"))?;
    let fallback = unique_output_path(&fallback_dir.join(name));
    std::fs::write(&fallback, transcript)
        .map_err(|e| anyhow!("écriture impossible dans Documents: {}", e))?;
    Ok(fallback)
}

fn dirs_documents() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Documents"))
}
```

- [ ] **Step 4 : Lancer les tests**

Run: `. "$HOME/.cargo/env" && cargo test --manifest-path src-tauri/Cargo.toml file_transcription::output 2>&1 | tail -20`
Expected: `test result: ok. 3 passed; 0 failed`

- [ ] **Step 5 : Commit**

```bash
git add src-tauri/src/managers/file_transcription/output.rs
git commit -m "feat: écriture du transcript sans jamais écraser l'existant

Une seconde transcription du même fichier ne doit pas détruire la première :
suffixage plutôt qu'écrasement silencieux."
```

---

## Task 5 : Manager et worker thread

C'est la tâche d'orchestration. Pas de test unitaire : elle dépend du `TranscriptionManager`, d'un modèle chargé et d'un `AppHandle`. Elle est validée par la recette manuelle de la Task 10.

**Files:**
- Modify: `src-tauri/src/managers/file_transcription/mod.rs`

- [ ] **Step 1 : Ajouter le manager**

Ajouter à la fin de `src-tauri/src/managers/file_transcription/mod.rs` :

```rust
use crate::audio_toolkit::vad::{SileroVad, VoiceActivityDetector};
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{get_settings, ModelUnloadTimeout};
use anyhow::{anyhow, Result};
use log::{error, info};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use tauri::{AppHandle, Manager};
use tauri_specta::Event;

pub struct FileTranscriptionManager {
    app: AppHandle,
    transcription: Arc<TranscriptionManager>,
    /// Jobs connus, dans l'ordre d'ajout. L'UI en est le reflet.
    jobs: Mutex<Vec<FileTranscriptionJob>>,
    queue: Mutex<VecDeque<String>>,
    queue_signal: Condvar,
    /// Ids annulés. Consulté entre deux blocs — l'inférence d'un bloc en cours
    /// n'est pas interruptible.
    cancelled: Mutex<Vec<String>>,
    next_id: AtomicU64,
    worker_started: AtomicBool,
}

impl FileTranscriptionManager {
    pub fn new(app: AppHandle, transcription: Arc<TranscriptionManager>) -> Self {
        Self {
            app,
            transcription,
            jobs: Mutex::new(Vec::new()),
            queue: Mutex::new(VecDeque::new()),
            queue_signal: Condvar::new(),
            cancelled: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
            worker_started: AtomicBool::new(false),
        }
    }

    pub fn list_jobs(&self) -> Vec<FileTranscriptionJob> {
        self.jobs.lock().unwrap().clone()
    }

    pub fn enqueue(self: &Arc<Self>, paths: Vec<PathBuf>) -> Result<Vec<FileTranscriptionJob>> {
        let mut created = Vec::new();

        for path in paths {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();
            if !SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
                return Err(anyhow!("format non supporté: .{}", ext));
            }

            let id = format!("job-{}", self.next_id.fetch_add(1, Ordering::SeqCst));
            let job = FileTranscriptionJob::new(id.clone(), path);
            self.jobs.lock().unwrap().push(job.clone());
            self.queue.lock().unwrap().push_back(id);
            created.push(job);
        }

        self.start_worker();
        self.queue_signal.notify_all();
        Ok(created)
    }

    pub fn cancel(&self, job_id: &str) {
        self.cancelled.lock().unwrap().push(job_id.to_string());
        // Un job encore en file n'a jamais été démarré : le marquer tout de suite
        // pour que l'UI réagisse sans attendre le worker.
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == job_id) {
            if job.status == JobStatus::Queued {
                job.status = JobStatus::Cancelled;
                let snapshot = job.clone();
                drop(jobs);
                self.emit(snapshot);
            }
        }
    }

    fn is_cancelled(&self, job_id: &str) -> bool {
        self.cancelled.lock().unwrap().iter().any(|id| id == job_id)
    }

    fn emit(&self, job: FileTranscriptionJob) {
        let _ = FileTranscriptionProgress { job }.emit(&self.app);
    }

    /// Applique une mutation à un job et pousse l'état résultant à l'UI.
    fn update<F: FnOnce(&mut FileTranscriptionJob)>(&self, job_id: &str, f: F) {
        let snapshot = {
            let mut jobs = self.jobs.lock().unwrap();
            let Some(job) = jobs.iter_mut().find(|j| j.id == job_id) else {
                return;
            };
            f(job);
            job.clone()
        };
        self.emit(snapshot);
    }

    fn start_worker(self: &Arc<Self>) {
        if self.worker_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("file-transcription".into())
            .spawn(move || me.worker_loop())
            .expect("failed to spawn file transcription worker");
    }

    fn worker_loop(self: Arc<Self>) {
        loop {
            let job_id = {
                let mut queue = self.queue.lock().unwrap();
                while queue.is_empty() {
                    queue = self.queue_signal.wait(queue).unwrap();
                }
                queue.pop_front().unwrap()
            };

            if self.is_cancelled(&job_id) {
                continue;
            }

            if let Err(e) = self.run_job(&job_id) {
                error!("file transcription job {} failed: {}", job_id, e);
                self.update(&job_id, |job| {
                    job.status = JobStatus::Failed;
                    job.error = Some(e.to_string());
                });
            }
        }
    }

    fn run_job(&self, job_id: &str) -> Result<()> {
        let source = {
            let jobs = self.jobs.lock().unwrap();
            let job = jobs
                .iter()
                .find(|j| j.id == job_id)
                .ok_or_else(|| anyhow!("job introuvable"))?;
            PathBuf::from(&job.source_path)
        };

        self.update(job_id, |job| job.status = JobStatus::Decoding);

        let samples = decode::decode_to_16k_mono(&source)?;
        let duration_secs = samples.len() as f64 / decode::TARGET_SAMPLE_RATE as f64;

        let mut vad = self.build_vad()?;
        let ranges = chunk::chunk_with_vad(&samples, vad.as_mut())?;

        self.update(job_id, |job| {
            job.status = JobStatus::Transcribing;
            job.chunks_total = ranges.len();
            job.duration_secs = duration_secs;
        });

        // Le modèle doit rester chargé d'un bloc à l'autre : sans ça, un
        // model_unload_timeout court le déchargerait entre deux blocs et
        // chaque bloc paierait un rechargement complet.
        let restore_timeout = self.suspend_unload_timeout();

        let model_id = get_settings(&self.app).selected_model;
        if model_id.is_empty() {
            restore_timeout(self);
            return Err(anyhow!(
                "aucun modèle sélectionné — choisissez-en un dans l'onglet Modèles"
            ));
        }
        if !self.transcription.is_model_loaded() {
            if let Err(e) = self.transcription.load_model_with_device(&model_id, None) {
                restore_timeout(self);
                return Err(anyhow!("chargement du modèle impossible: {}", e));
            }
        }

        let mut pieces: Vec<String> = Vec::with_capacity(ranges.len());
        for (i, range) in ranges.iter().enumerate() {
            if self.is_cancelled(job_id) {
                restore_timeout(self);
                self.update(job_id, |job| job.status = JobStatus::Cancelled);
                return Ok(());
            }

            // Priorité à la dictée : si l'utilisateur enregistre au micro,
            // attendre avant d'attaquer le bloc suivant. Sans ça, un fichier
            // d'une heure en fond rendrait le raccourci inutilisable, chaque
            // dictée devant attendre la fin d'un bloc de 2 min.
            self.wait_while_recording();

            let chunk = samples[range.clone()].to_vec();
            match self.transcription.transcribe(chunk) {
                Ok(text) => {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        pieces.push(trimmed.to_string());
                    }
                }
                Err(e) => {
                    restore_timeout(self);
                    return Err(anyhow!("bloc {}/{}: {}", i + 1, ranges.len(), e));
                }
            }

            self.update(job_id, |job| job.chunks_done = i + 1);
        }

        restore_timeout(self);

        let transcript = pieces.join(" ");
        let written = output::write_transcript(&source, &transcript);

        self.update(job_id, |job| {
            job.status = JobStatus::Done;
            job.transcript = Some(transcript.clone());
            match &written {
                Ok(path) => job.output_path = Some(path.to_string_lossy().to_string()),
                // Le transcript reste affiché : un disque plein ne doit pas
                // effacer 40 minutes de calcul.
                Err(e) => job.error = Some(format!("transcript non écrit sur disque: {}", e)),
            }
        });

        info!("file transcription job {} done", job_id);
        Ok(())
    }

    /// Bloque tant qu'un enregistrement micro est en cours, avec un plafond de
    /// sécurité : un état d'enregistrement resté coincé ne doit pas figer la
    /// file de fichiers pour toujours.
    fn wait_while_recording(&self) {
        use crate::managers::audio::AudioRecordingManager;

        let audio = self.app.state::<Arc<AudioRecordingManager>>();
        let mut waited = std::time::Duration::ZERO;
        let step = std::time::Duration::from_millis(200);
        let cap = std::time::Duration::from_secs(120);

        while audio.is_recording() && waited < cap {
            std::thread::sleep(step);
            waited += step;
        }
    }

    fn build_vad(&self) -> Result<Box<dyn VoiceActivityDetector>> {
        let model_path = self
            .app
            .path()
            .resolve(
                "resources/models/silero_vad_v4.onnx",
                tauri::path::BaseDirectory::Resource,
            )
            .map_err(|e| anyhow!("modèle VAD introuvable: {}", e))?;
        let vad = SileroVad::new(model_path, 0.5)
            .map_err(|e| anyhow!("initialisation du VAD impossible: {}", e))?;
        Ok(Box::new(vad))
    }

    /// Force le timeout de déchargement du modèle sur une valeur longue et
    /// rend une closure qui restaure la valeur d'origine.
    fn suspend_unload_timeout(&self) -> impl FnOnce(&Self) {
        let mut settings = get_settings(&self.app);
        let previous = settings.model_unload_timeout;
        settings.model_unload_timeout = ModelUnloadTimeout::Never;
        crate::settings::write_settings(&self.app, settings);

        move |me: &Self| {
            let mut settings = get_settings(&me.app);
            settings.model_unload_timeout = previous;
            crate::settings::write_settings(&me.app, settings);
        }
    }
}
```

- [ ] **Step 2 : Vérifier la variante d'énumération**

`ModelUnloadTimeout::Never` doit exister. Vérifier :

Run: `grep -n -A 12 "pub enum ModelUnloadTimeout" src-tauri/src/settings.rs`
Si la variante « jamais décharger » porte un autre nom, utiliser celui-ci dans `suspend_unload_timeout`.

- [ ] **Step 3 : Compiler**

Run: `. "$HOME/.cargo/env" && cargo check --manifest-path src-tauri/Cargo.toml 2>&1 | tail -30`
Expected: `Finished`. Corriger les erreurs de signature en se référant au code existant — `managers/transcription.rs` fait autorité sur `load_model_with_device`, `is_model_loaded` et `transcribe`.

- [ ] **Step 4 : Commit**

```bash
git add src-tauri/src/managers/file_transcription/mod.rs
git commit -m "feat: worker de transcription de fichiers

Un seul job à la fois : le modèle ne peut pas se dédoubler, et paralléliser
ne ferait que se disputer le même GPU."
```

---

## Task 6 : Commandes Tauri et câblage

**Files:**
- Create: `src-tauri/src/commands/file_transcription.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1 : Écrire les commandes**

Créer `src-tauri/src/commands/file_transcription.rs` :

```rust
use crate::managers::file_transcription::{FileTranscriptionJob, FileTranscriptionManager};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;
use tauri_plugin_opener::OpenerExt;

#[tauri::command]
#[specta::specta]
pub fn enqueue_file_transcriptions(
    manager: State<Arc<FileTranscriptionManager>>,
    paths: Vec<String>,
) -> Result<Vec<FileTranscriptionJob>, String> {
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    manager.enqueue(paths).map_err(|e| e.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn cancel_file_transcription(
    manager: State<Arc<FileTranscriptionManager>>,
    job_id: String,
) {
    manager.cancel(&job_id);
}

#[tauri::command]
#[specta::specta]
pub fn list_file_transcription_jobs(
    manager: State<Arc<FileTranscriptionManager>>,
) -> Vec<FileTranscriptionJob> {
    manager.list_jobs()
}

/// Révèle le `.txt` dans le Finder / l'explorateur. Passer par le backend
/// évite d'avoir à ouvrir une permission `fs:scope` sur tout le disque côté
/// frontend.
#[tauri::command]
#[specta::specta]
pub fn reveal_transcript_file(app: tauri::AppHandle, path: String) -> Result<(), String> {
    app.opener()
        .reveal_item_in_dir(path)
        .map_err(|e| format!("impossible de révéler le fichier: {}", e))
}
```

- [ ] **Step 2 : Déclarer le module de commandes**

Dans `src-tauri/src/commands/mod.rs`, ajouter avec les autres `pub mod` (fichier en tête) :

```rust
pub mod file_transcription;
```

- [ ] **Step 3 : Instancier le manager**

Dans `src-tauri/src/lib.rs`, après la création de `history_manager` (autour de la ligne 169) :

```rust
    let file_transcription_manager = Arc::new(
        managers::file_transcription::FileTranscriptionManager::new(
            app_handle.clone(),
            transcription_manager.clone(),
        ),
    );
```

Puis, dans le bloc `app_handle.manage(...)` (autour de la ligne 182), après `app_handle.manage(history_manager.clone());` :

```rust
    app_handle.manage(file_transcription_manager.clone());
```

- [ ] **Step 4 : Enregistrer les commandes et l'événement**

Dans `src-tauri/src/lib.rs`, dans `collect_commands![...]`, après `commands::history::update_recording_retention_period,` :

```rust
            commands::file_transcription::enqueue_file_transcriptions,
            commands::file_transcription::cancel_file_transcription,
            commands::file_transcription::list_file_transcription_jobs,
            commands::file_transcription::reveal_transcript_file,
```

Dans `collect_events![...]`, après `managers::transcription::StreamPhaseEvent,` :

```rust
            managers::file_transcription::FileTranscriptionProgress,
```

- [ ] **Step 5 : Compiler et régénérer les bindings**

Run: `. "$HOME/.cargo/env" && cargo check --manifest-path src-tauri/Cargo.toml 2>&1 | tail -30`
Expected: `Finished`

Puis lancer l'app une fois en dev pour régénérer `src/bindings.ts` (l'export ne se fait qu'en build debug) :

Run: `. "$HOME/.cargo/env" && CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri dev`
Attendre l'ouverture de la fenêtre, puis quitter (Ctrl+C).

Run: `grep -n "enqueueFileTranscriptions\|FileTranscriptionProgress" src/bindings.ts`
Expected: les deux symboles apparaissent. Sinon, l'export specta n'a pas tourné — vérifier que la commande est bien dans `collect_commands!`.

- [ ] **Step 6 : Commit**

```bash
git add src-tauri/src/commands/file_transcription.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs src/bindings.ts
git commit -m "feat: commandes Tauri de transcription de fichiers

reveal_item_in_dir passe par le backend pour ne pas avoir à ouvrir une
permission fs sur tout le disque côté frontend."
```

---

## Task 7 : Store frontend

**Files:**
- Create: `src/stores/fileTranscriptionStore.ts`

- [ ] **Step 1 : Écrire le store**

Créer `src/stores/fileTranscriptionStore.ts` :

```typescript
import { create } from "zustand";
import { commands, events, type FileTranscriptionJob } from "../bindings";

interface FileTranscriptionState {
  jobs: FileTranscriptionJob[];
  selectedJobId: string | null;
  /** Message d'erreur de mise en file (format non supporté, etc.). */
  enqueueError: string | null;
  refresh: () => Promise<void>;
  enqueue: (paths: string[]) => Promise<void>;
  cancel: (jobId: string) => Promise<void>;
  select: (jobId: string | null) => void;
  clearEnqueueError: () => void;
  subscribe: () => Promise<() => void>;
}

export const useFileTranscriptionStore = create<FileTranscriptionState>(
  (set, get) => ({
    jobs: [],
    selectedJobId: null,
    enqueueError: null,

    refresh: async () => {
      const jobs = await commands.listFileTranscriptionJobs();
      set({ jobs });
    },

    enqueue: async (paths) => {
      set({ enqueueError: null });
      const result = await commands.enqueueFileTranscriptions(paths);
      if (result.status === "error") {
        set({ enqueueError: result.error });
        return;
      }
      await get().refresh();
      // Sélectionner le premier fichier ajouté évite un panneau de résultat
      // vide juste après un dépôt.
      const first = result.data[0];
      if (first && !get().selectedJobId) {
        set({ selectedJobId: first.id });
      }
    },

    cancel: async (jobId) => {
      await commands.cancelFileTranscription(jobId);
      await get().refresh();
    },

    select: (jobId) => set({ selectedJobId: jobId }),

    clearEnqueueError: () => set({ enqueueError: null }),

    subscribe: async () => {
      const unlisten = await events.fileTranscriptionProgress.listen((e) => {
        const incoming = e.payload.job;
        set((state) => {
          const jobs = state.jobs.some((j) => j.id === incoming.id)
            ? state.jobs.map((j) => (j.id === incoming.id ? incoming : j))
            : [...state.jobs, incoming];
          return { jobs };
        });
      });
      return unlisten;
    },
  }),
);
```

- [ ] **Step 2 : Vérifier les types**

Run: `bun run build 2>&1 | tail -20`
Expected: build TypeScript sans erreur.

Les noms exacts exportés par `bindings.ts` (`commands.enqueueFileTranscriptions`, `events.fileTranscriptionProgress`) sont générés par specta en camelCase. Si la compilation échoue sur un nom, ouvrir `src/bindings.ts` et utiliser le nom réellement généré — le fichier généré fait autorité.

- [ ] **Step 3 : Commit**

```bash
git add src/stores/fileTranscriptionStore.ts
git commit -m "feat: store des jobs de transcription de fichiers"
```

---

## Task 8 : Composants d'interface

**Files:**
- Create: `src/components/settings/files/FileDropZone.tsx`
- Create: `src/components/settings/files/FileJobRow.tsx`
- Create: `src/components/settings/files/FilesSettings.tsx`
- Create: `src/components/settings/files/index.ts`
- Modify: `src/components/settings/index.ts`

- [ ] **Step 1 : Zone de dépôt**

Créer `src/components/settings/files/FileDropZone.tsx` :

```tsx
import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Upload } from "lucide-react";
import { Button } from "../../ui";

const SUPPORTED_EXTENSIONS = [
  "m4a",
  "mp4",
  "mp3",
  "wav",
  "flac",
  "ogg",
  "oga",
  "aac",
  "caf",
];

interface FileDropZoneProps {
  onFiles: (paths: string[]) => void;
}

export const FileDropZone: React.FC<FileDropZoneProps> = ({ onFiles }) => {
  const { t } = useTranslation();
  const [isHovering, setIsHovering] = useState(false);

  useEffect(() => {
    let unlisten: (() => void) | undefined;

    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "over") {
          setIsHovering(true);
        } else if (event.payload.type === "drop") {
          setIsHovering(false);
          onFiles(event.payload.paths);
        } else {
          setIsHovering(false);
        }
      })
      .then((fn) => {
        unlisten = fn;
      });

    return () => unlisten?.();
  }, [onFiles]);

  const handleBrowse = useCallback(async () => {
    const selected = await open({
      multiple: true,
      filters: [
        { name: t("files.audioFilter"), extensions: SUPPORTED_EXTENSIONS },
      ],
    });
    if (!selected) return;
    onFiles(Array.isArray(selected) ? selected : [selected]);
  }, [onFiles, t]);

  return (
    <div
      className={`flex flex-col items-center justify-center gap-3 p-8 rounded-lg border-2 border-dashed transition-colors ${
        isHovering
          ? "border-logo-primary bg-logo-primary/10"
          : "border-mid-gray/30"
      }`}
    >
      <Upload width={28} height={28} className="opacity-60" />
      <p className="text-sm text-text/70 text-center">{t("files.dropHint")}</p>
      <Button variant="secondary" size="sm" onClick={handleBrowse}>
        {t("files.browse")}
      </Button>
    </div>
  );
};
```

- [ ] **Step 2 : Ligne de la file d'attente**

Créer `src/components/settings/files/FileJobRow.tsx` :

```tsx
import React from "react";
import { useTranslation } from "react-i18next";
import { X } from "lucide-react";
import type { FileTranscriptionJob } from "../../../bindings";
import ProgressBar from "../../shared/ProgressBar";

interface FileJobRowProps {
  job: FileTranscriptionJob;
  isSelected: boolean;
  onSelect: () => void;
  onCancel: () => void;
}

const isRunning = (job: FileTranscriptionJob) =>
  job.status === "queued" ||
  job.status === "decoding" ||
  job.status === "transcribing";

const formatDuration = (secs: number): string => {
  const total = Math.round(secs);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
};

export const FileJobRow: React.FC<FileJobRowProps> = ({
  job,
  isSelected,
  onSelect,
  onCancel,
}) => {
  const { t } = useTranslation();

  const percentage =
    job.chunks_total > 0 ? (job.chunks_done / job.chunks_total) * 100 : 0;

  return (
    <div
      className={`flex items-center gap-3 p-3 rounded-lg cursor-pointer transition-colors ${
        isSelected ? "bg-logo-primary/20" : "hover:bg-mid-gray/10"
      }`}
      onClick={onSelect}
    >
      <div className="flex-1 min-w-0">
        <p className="text-sm font-medium truncate" title={job.file_name}>
          {job.file_name}
        </p>
        <p className="text-xs text-text/60">
          {job.duration_secs > 0 && (
            <span className="me-2">{formatDuration(job.duration_secs)}</span>
          )}
          <span>{t(`files.status.${job.status}`)}</span>
        </p>
      </div>

      {job.status === "transcribing" && (
        <ProgressBar
          progress={[{ id: job.id, percentage }]}
          size="medium"
          showLabel
        />
      )}

      {isRunning(job) && (
        <button
          className="p-1 rounded hover:bg-mid-gray/20 shrink-0"
          title={t("files.cancel")}
          onClick={(e) => {
            e.stopPropagation();
            onCancel();
          }}
        >
          <X width={16} height={16} />
        </button>
      )}
    </div>
  );
};
```

- [ ] **Step 3 : Écran principal**

Créer `src/components/settings/files/FilesSettings.tsx` :

```tsx
import React, { useCallback, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { toast } from "sonner";
import { commands } from "../../../bindings";
import { useFileTranscriptionStore } from "../../../stores/fileTranscriptionStore";
import { SettingsGroup, Button, Alert } from "../../ui";
import { FileDropZone } from "./FileDropZone";
import { FileJobRow } from "./FileJobRow";

export const FilesSettings: React.FC = () => {
  const { t } = useTranslation();
  const {
    jobs,
    selectedJobId,
    enqueueError,
    refresh,
    enqueue,
    cancel,
    select,
    clearEnqueueError,
    subscribe,
  } = useFileTranscriptionStore();

  useEffect(() => {
    refresh();
    let unlisten: (() => void) | undefined;
    subscribe().then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, [refresh, subscribe]);

  const handleFiles = useCallback(
    (paths: string[]) => {
      enqueue(paths);
    },
    [enqueue],
  );

  const selectedJob = jobs.find((j) => j.id === selectedJobId) ?? null;

  const handleCopy = useCallback(async () => {
    if (!selectedJob?.transcript) return;
    await writeText(selectedJob.transcript);
    toast.success(t("files.copied"));
  }, [selectedJob, t]);

  const handleReveal = useCallback(async () => {
    if (!selectedJob?.output_path) return;
    await commands.revealTranscriptFile(selectedJob.output_path);
  }, [selectedJob]);

  return (
    <div className="flex flex-col gap-6 p-6 overflow-y-auto">
      <SettingsGroup title={t("files.title")}>
        <div className="p-4">
          <FileDropZone onFiles={handleFiles} />
        </div>
      </SettingsGroup>

      {enqueueError && (
        <Alert variant="error" onClose={clearEnqueueError}>
          {enqueueError}
        </Alert>
      )}

      {jobs.length > 0 && (
        <SettingsGroup title={t("files.queue")}>
          <div className="flex flex-col p-2">
            {jobs.map((job) => (
              <FileJobRow
                key={job.id}
                job={job}
                isSelected={job.id === selectedJobId}
                onSelect={() => select(job.id)}
                onCancel={() => cancel(job.id)}
              />
            ))}
          </div>
        </SettingsGroup>
      )}

      {selectedJob?.status === "done" && (
        <SettingsGroup title={t("files.result")}>
          <div className="flex flex-col gap-3 p-4">
            <textarea
              readOnly
              value={selectedJob.transcript ?? ""}
              className="w-full h-64 p-3 text-sm rounded-lg bg-mid-gray/10 resize-none"
            />
            <div className="flex gap-2">
              <Button variant="secondary" size="sm" onClick={handleCopy}>
                {t("files.copy")}
              </Button>
              {selectedJob.output_path && (
                <Button variant="secondary" size="sm" onClick={handleReveal}>
                  {t("files.reveal")}
                </Button>
              )}
            </div>
            {selectedJob.error && (
              <p className="text-xs text-red-500">{selectedJob.error}</p>
            )}
          </div>
        </SettingsGroup>
      )}

      {selectedJob?.status === "failed" && (
        <Alert variant="error">{selectedJob.error ?? t("files.genericError")}</Alert>
      )}
    </div>
  );
};
```

- [ ] **Step 4 : Ré-exports**

Créer `src/components/settings/files/index.ts` :

```typescript
export { FilesSettings } from "./FilesSettings";
export { FileDropZone } from "./FileDropZone";
export { FileJobRow } from "./FileJobRow";
```

Dans `src/components/settings/index.ts`, ajouter la ligne, en suivant le style des exports existants du fichier :

```typescript
export { FilesSettings } from "./files";
```

- [ ] **Step 5 : Vérifier les API des composants réutilisés**

Les props `variant`/`size` de `Button`, `title` de `SettingsGroup` et `variant`/`onClose` de `Alert` sont supposées d'après leur usage ailleurs dans le projet. Les confirmer :

Run: `grep -n "interface ButtonProps" -A 12 src/components/ui/Button.tsx && grep -n "interface AlertProps" -A 10 src/components/ui/Alert.tsx && grep -n "interface SettingsGroupProps" -A 8 src/components/ui/SettingsGroup.tsx`

Adapter les appels aux signatures réelles.

- [ ] **Step 6 : Compiler et linter**

Run: `bun run build 2>&1 | tail -20 && bun run lint 2>&1 | tail -20`
Expected: aucune erreur. Le lint i18next doit être silencieux — toute chaîne littérale restante dans le JSX est une erreur à corriger.

- [ ] **Step 7 : Commit**

```bash
git add src/components/settings/files/ src/components/settings/index.ts
git commit -m "feat: interface de l'onglet Fichiers"
```

---

## Task 9 : Entrée de navigation et traductions

**Files:**
- Modify: `src/components/Sidebar.tsx`
- Modify: `src/i18n/locales/en/translation.json`
- Modify: `src/i18n/locales/fr/translation.json`

- [ ] **Step 1 : Ajouter l'entrée de sidebar**

Dans `src/components/Sidebar.tsx` :

Ligne 3, ajouter `FileAudio` aux imports lucide :

```tsx
import { Cog, FileAudio, FlaskConical, History, Info, Sparkles, Cpu } from "lucide-react";
```

Ligne 8-15, ajouter `FilesSettings` à l'import depuis `./settings`.

Dans `SECTIONS_CONFIG`, insérer entre `history` et `models` :

```tsx
  files: {
    labelKey: "sidebar.files",
    icon: FileAudio,
    component: FilesSettings,
    enabled: () => true,
  },
```

- [ ] **Step 2 : Traductions anglaises**

Dans `src/i18n/locales/en/translation.json`, ajouter la clé `sidebar.files` à l'objet `sidebar` existant :

```json
    "files": "Files"
```

Et ajouter un bloc `files` de premier niveau :

```json
  "files": {
    "title": "Transcribe audio files",
    "dropHint": "Drop call recordings here (m4a, mp3, mp4, wav, flac, ogg)",
    "browse": "Browse…",
    "audioFilter": "Audio files",
    "queue": "Queue",
    "result": "Transcript",
    "copy": "Copy",
    "copied": "Transcript copied",
    "reveal": "Show file",
    "cancel": "Cancel",
    "genericError": "Transcription failed",
    "status": {
      "queued": "Waiting",
      "decoding": "Decoding…",
      "transcribing": "Transcribing…",
      "done": "Done",
      "failed": "Failed",
      "cancelled": "Cancelled"
    }
  }
```

- [ ] **Step 3 : Traductions françaises**

Dans `src/i18n/locales/fr/translation.json`, ajouter à l'objet `sidebar` :

```json
    "files": "Fichiers"
```

Et le bloc `files` de premier niveau :

```json
  "files": {
    "title": "Transcrire des fichiers audio",
    "dropHint": "Déposez vos enregistrements de call ici (m4a, mp3, mp4, wav, flac, ogg)",
    "browse": "Parcourir…",
    "audioFilter": "Fichiers audio",
    "queue": "File d'attente",
    "result": "Transcript",
    "copy": "Copier",
    "copied": "Transcript copié",
    "reveal": "Afficher le fichier",
    "cancel": "Annuler",
    "genericError": "Échec de la transcription",
    "status": {
      "queued": "En attente",
      "decoding": "Décodage…",
      "transcribing": "Transcription…",
      "done": "Terminé",
      "failed": "Échec",
      "cancelled": "Annulé"
    }
  }
```

- [ ] **Step 4 : Vérifier**

Run: `bun run build 2>&1 | tail -10 && bun run lint 2>&1 | tail -10`
Expected: aucune erreur.

Run: `python3 -c "import json; [json.load(open(f'src/i18n/locales/{l}/translation.json')) for l in ['en','fr']]; print('JSON valide')"`
Expected: `JSON valide`

- [ ] **Step 5 : Commit**

```bash
git add src/components/Sidebar.tsx src/i18n/locales/en/translation.json src/i18n/locales/fr/translation.json
git commit -m "feat: onglet Fichiers dans la navigation + traductions en/fr"
```

---

## Task 10 : Recette manuelle et build final

Aucun test automatisé ne couvre le chemin complet : il exige un modèle de plusieurs Go et de vrais fichiers audio. Cette recette est la vérification qui compte.

**Prérequis :** un modèle de transcription installé (onglet Modèles) et au moins un enregistrement de call réel en `.m4a`.

- [ ] **Step 1 : Lancer tous les tests unitaires**

Run: `. "$HOME/.cargo/env" && cargo test --manifest-path src-tauri/Cargo.toml file_transcription 2>&1 | tail -20`
Expected: `test result: ok. 11 passed; 0 failed`

- [ ] **Step 2 : Lancer en dev**

Run: `. "$HOME/.cargo/env" && CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri dev`

- [ ] **Step 3 : Recette fonctionnelle**

Cocher chaque point :

- [ ] L'onglet « Fichiers » apparaît dans la sidebar, entre Historique et Modèles
- [ ] Déposer un `.m4a` de call : le statut passe `Décodage…` puis `Transcription…`
- [ ] La barre de progression avance par paliers réguliers, sans rester figée
- [ ] En fin de job : statut `Terminé`, transcript affiché, `.txt` présent à côté du fichier source
- [ ] Le contenu du `.txt` correspond au transcript affiché
- [ ] Bouton « Copier » : le presse-papier contient bien le transcript
- [ ] Bouton « Afficher le fichier » : le Finder s'ouvre sur le `.txt`
- [ ] Déposer un `.txt` ou un `.pdf` : rejet avec message, aucun job créé
- [ ] Déposer deux fichiers d'un coup : traitement séquentiel, pas simultané
- [ ] Annuler un job en cours : passe à `Annulé`, aucun `.txt` écrit
- [ ] Pendant un job, déclencher la dictée au raccourci : le texte est collé normalement, le job reprend ensuite
- [ ] Re-transcrire le même fichier : produit `nom-2.txt`, le premier `.txt` est intact
- [ ] Après le job, vérifier dans les réglages que `model_unload_timeout` est revenu à sa valeur d'origine (5 min)

- [ ] **Step 4 : Build de production**

Run: `. "$HOME/.cargo/env" && CMAKE_POLICY_VERSION_MINIMUM=3.5 bun run tauri build 2>&1 | tail -30`
Expected: `Built application at:` puis `Bundling Handy.app`. L'échec final sur `TAURI_SIGNING_PRIVATE_KEY` est attendu et sans conséquence.

Run: `ls -la src-tauri/target/release/bundle/macos/Handy.app/Contents/MacOS/handy`
Expected: le binaire existe, daté de maintenant.

- [ ] **Step 5 : Installer**

```bash
# Quitter Handy avant de remplacer le bundle.
osascript -e 'quit app "Handy"' || true
rm -rf /Applications/Handy.app
cp -R src-tauri/target/release/bundle/macos/Handy.app /Applications/
xattr -dr com.apple.quarantine /Applications/Handy.app
open /Applications/Handy.app
```

Vérifier que l'app démarre, que les réglages précédents (raccourci `fn`, micro, langue) sont intacts, et que l'onglet Fichiers est là.

- [ ] **Step 6 : Commit final**

```bash
git add -A
git commit -m "chore: recette de la transcription de fichiers"
```

---

## Notes pour l'implémenteur

**Si le décodage `.m4a` échoue.** Le support AAC de `symphonia` est le maillon le moins mature de la chaîne. Repli macOS : convertir en amont avec `afconvert`, binaire système présent sur tout Mac, avant de passer par le décodeur WAV.

```rust
// Repli si symphonia refuse un m4a : conversion préalable via afconvert.
std::process::Command::new("/usr/bin/afconvert")
    .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1", src, dst])
    .status()?;
```

**Si `FrameResampler` ne convient pas au traitement par lot.** Il est conçu pour un flux temps réel par frames. S'il se révèle inadapté à un signal complet en mémoire, utiliser directement `rubato::FftFixedIn` — la crate est déjà une dépendance du projet.

**Ordre des tâches.** Les tâches 2, 3 et 4 sont indépendantes et peuvent être faites dans n'importe quel ordre. La tâche 5 dépend des trois. Les tâches 7 à 9 dépendent de la 6 pour les bindings générés.
