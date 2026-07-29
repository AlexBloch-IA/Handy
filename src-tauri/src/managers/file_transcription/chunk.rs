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

            if run_len >= MIN_SILENCE_FRAMES && cut > start && cut - start >= TARGET_CHUNK_SAMPLES {
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

        assert!(
            ranges.len() >= 2,
            "attendu une coupe forcée, obtenu {:?}",
            ranges
        );
        assert!(
            ranges
                .iter()
                .all(|r| r.len() <= MAX_CHUNK_SAMPLES + FRAME_SAMPLES),
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
