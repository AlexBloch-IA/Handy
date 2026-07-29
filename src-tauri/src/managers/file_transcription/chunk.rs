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
/// Longueur plancher d'un bloc de fin : 5 s.
///
/// Whisper part en roue libre sur les buffers très courts et invente une
/// formule de politesse ("Thank you.", "Sous-titres réalisés par…") qui vient
/// ensuite polluer le transcript. 5 s laisse largement de quoi porter du
/// contenu réel tout en restant négligeable devant la cible de 2 min.
const MIN_TAIL_SAMPLES: usize = 5 * 16_000;

/// Longueur minimale d'un bloc issu d'une coupe : 0,75 × la cible.
///
/// Un silence qui tombe trop tôt n'est pas une bonne frontière : il hacherait
/// la transcription en blocs courts et multiplierait les appels au moteur.
const MIN_CHUNK_SAMPLES: usize = TARGET_CHUNK_SAMPLES * 3 / 4;

/// Marge de silence conservée de part et d'autre d'une coupe.
///
/// On ne coupe jamais au ras d'une frontière : une queue de silence pour le
/// bloc qui se termine, une amorce pour celui qui commence.
const SILENCE_MARGIN_FRAMES: usize = MIN_SILENCE_FRAMES / 2;

/// Plage d'échantillons où l'on peut couper sans toucher à la parole,
/// c'est-à-dire un silence assez long, amputé de ses marges.
struct CutWindow {
    lo: usize,
    hi: usize,
}

/// Repère les silences exploitables et rend, pour chacun, la plage de coupe
/// correspondante.
fn cut_windows(speech: &[bool]) -> Vec<CutWindow> {
    let mut windows = Vec::new();
    let mut i = 0usize;

    while i < speech.len() {
        if speech[i] {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < speech.len() && !speech[i] {
            i += 1;
        }
        if i - run_start >= MIN_SILENCE_FRAMES {
            windows.push(CutWindow {
                lo: (run_start + SILENCE_MARGIN_FRAMES) * FRAME_SAMPLES,
                hi: (i - SILENCE_MARGIN_FRAMES) * FRAME_SAMPLES,
            });
        }
    }

    windows
}

/// Découpe un masque parole/silence en intervalles d'échantillons contigus.
///
/// `speech[i]` décrit la frame `i` de `FRAME_SAMPLES` échantillons.
/// Les intervalles rendus sont contigus et couvrent `0..total_samples`.
///
/// Règles, par ordre de priorité : aucun échantillon perdu ; couper dans un
/// silence dès qu'il en existe un proche de la cible ; ne jamais dépasser la
/// limite dure, silence ou pas ; ne jamais rendre une miette en fin de
/// signal, sauf si le signal entier est court.
pub fn chunk_ranges(speech: &[bool], total_samples: usize) -> Vec<Range<usize>> {
    if speech.is_empty() || total_samples == 0 {
        return Vec::new();
    }

    // Un appelant qui annonce moins d'échantillons que le masque n'en décrit
    // ne doit pas récupérer d'intervalle hors bornes : `samples[range]`
    // paniquerait. On ignore les frames au-delà du signal déclaré.
    let frames = speech.len().min(total_samples.div_ceil(FRAME_SAMPLES));
    let windows = cut_windows(&speech[..frames]);

    let mut ranges = Vec::new();
    let mut start = 0usize;

    while start < total_samples {
        match best_cut(&windows, start, total_samples) {
            Some(cut) => {
                ranges.push(start..cut);
                start = cut;
            }
            // Aucun silence exploitable et la limite dure est franchie : on
            // coupe au milieu de la parole, à contrecœur.
            None if total_samples - start > MAX_CHUNK_SAMPLES => {
                let remaining = total_samples - start;
                // Couper pile à la limite laisserait parfois une miette de
                // quelques frames derrière ; à ce compte-là, autant partager
                // le reste en deux blocs équilibrés.
                let cut = if remaining <= MAX_CHUNK_SAMPLES + MIN_TAIL_SAMPLES {
                    start + remaining / 2
                } else {
                    start + MAX_CHUNK_SAMPLES
                };
                ranges.push(start..cut);
                start = cut;
            }
            None => {
                ranges.push(start..total_samples);
                break;
            }
        }
    }

    ranges
}

/// Meilleure coupe pour le bloc qui commence à `start` : celle qui, parmi les
/// silences acceptables, tombe au plus près de la cible.
///
/// Rend `None` si aucun silence ne convient — au bloc appelant de décider s'il
/// force une coupe ou s'il s'arrête là.
fn best_cut(windows: &[CutWindow], start: usize, total_samples: usize) -> Option<usize> {
    let lo_bound = start + MIN_CHUNK_SAMPLES;
    // Ni au-delà de la limite dure, ni assez près de la fin pour condamner le
    // dernier bloc à n'être qu'un fragment.
    let hi_bound = (start + MAX_CHUNK_SAMPLES).min(total_samples.saturating_sub(MIN_TAIL_SAMPLES));
    if hi_bound < lo_bound {
        return None;
    }
    let target = start + TARGET_CHUNK_SAMPLES;

    windows
        .iter()
        .filter(|w| w.hi >= lo_bound && w.lo <= hi_bound)
        // Un long silence offre tout un intervalle de coupes valables : on y
        // prend le point le plus proche de la cible, pas son milieu.
        .map(|w| target.clamp(w.lo.max(lo_bound), w.hi.min(hi_bound)))
        .min_by_key(|cut| cut.abs_diff(target))
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

    /// Vérifie l'invariant central : aucun bloc au-delà de la limite dure.
    fn assert_cap_respected(ranges: &[Range<usize>]) {
        for r in ranges {
            assert!(
                r.len() <= MAX_CHUNK_SAMPLES + FRAME_SAMPLES,
                "bloc de {} échantillons ({} s) au-dessus de la limite dure, blocs = {:?}",
                r.len(),
                r.len() / 16_000,
                ranges.iter().map(|r| r.len() / 16_000).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn silence_never_escapes_the_hard_cap() {
        // Micro coupé / mauvais périphérique : 60 min de silence intégral.
        // Sans coupe dans le silence, Whisper reçoit 30 min d'un coup et
        // l'utilisateur ne voit aucune progression pendant tout ce temps.
        let m = mask(&[(frames_for_secs(3600), false)]);
        let total = m.len() * FRAME_SAMPLES;

        let ranges = chunk_ranges(&m, total);

        assert_cap_respected(&ranges);
        assert_eq!(ranges.last().unwrap().end, total);
    }

    #[test]
    fn a_long_pause_between_two_speech_runs_stays_under_the_cap() {
        // 100 s de parole, 8 min de pause, 100 s de parole.
        let m = mask(&[
            (frames_for_secs(100), true),
            (frames_for_secs(480), false),
            (frames_for_secs(100), true),
        ]);
        let total = m.len() * FRAME_SAMPLES;

        let ranges = chunk_ranges(&m, total);

        assert_cap_respected(&ranges);
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, total);
    }

    #[test]
    fn never_leaves_a_scrap_chunk_at_the_end() {
        // 180,03 s de parole continue : la coupe forcée à 180 s laisse un
        // résidu de 30 ms, que Whisper transcrit en hallucination.
        let m = mask(&[(6001, true)]);
        let total = m.len() * FRAME_SAMPLES;
        assert_eq!(total, MAX_CHUNK_SAMPLES + FRAME_SAMPLES);

        let ranges = chunk_ranges(&m, total);

        assert!(ranges.len() >= 2, "attendu une coupe, obtenu {:?}", ranges);
        for r in &ranges {
            assert!(
                r.len() >= MIN_TAIL_SAMPLES,
                "bloc de {} échantillons, sous le plancher de {} : {:?}",
                r.len(),
                MIN_TAIL_SAMPLES,
                ranges
            );
        }
        assert_cap_respected(&ranges);
    }

    #[test]
    fn a_genuinely_short_file_still_yields_one_chunk() {
        // Le plancher ne doit pas faire disparaître un fichier réellement
        // court : 2 s de parole restent un bloc de 2 s.
        let m = mask(&[(frames_for_secs(2), true)]);
        let total = m.len() * FRAME_SAMPLES;

        let ranges = chunk_ranges(&m, total);

        assert_eq!(ranges, vec![0..total]);
    }

    #[test]
    fn prefers_the_silence_closest_to_the_target_over_a_forced_cut() {
        // 119 s de parole, 1 s de silence, 120 s de parole.
        // Le silence est à 1 s de la cible : couper à 180 s au milieu d'un mot
        // alors qu'une frontière parfaite existe est exactement le défaut que
        // cette fonction doit éviter.
        let speech_a = frames_for_secs(119);
        let silence = frames_for_secs(1);
        let m = mask(&[
            (speech_a, true),
            (silence, false),
            (frames_for_secs(120), true),
        ]);
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

    /// Balayage sur des masques de forme réaliste (alternance parole /
    /// respirations, 30 à 60 min). Les invariants vérifiés ici sont ceux dont
    /// dépend l'appelant : il indexe `samples[range]` et concatène les
    /// transcripts dans l'ordre.
    #[test]
    fn invariants_hold_on_call_shaped_masks() {
        let mut seed = 0x2545F4914F6CDD1Du64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut short_tails = 0usize;
        for case in 0..500 {
            let mut m: Vec<bool> = Vec::new();
            let target_frames = frames_for_secs(1800 + (case % 1800));
            let mut speech = true;
            while m.len() < target_frames {
                let len = if speech {
                    1 + (rand() % 900) as usize
                } else {
                    1 + (rand() % 120) as usize
                };
                m.extend(std::iter::repeat(speech).take(len));
                speech = !speech;
            }
            let total = m.len() * FRAME_SAMPLES;
            let ranges = chunk_ranges(&m, total);
            assert_eq!(ranges.first().unwrap().start, 0);
            assert_eq!(ranges.last().unwrap().end, total);
            for pair in ranges.windows(2) {
                assert_eq!(pair[0].end, pair[1].start);
            }
            for r in &ranges {
                assert!(r.start < r.end, "intervalle vide/inversé {:?}", r);
                assert!(
                    r.len() <= MAX_CHUNK_SAMPLES + FRAME_SAMPLES,
                    "cap dépassé {:?}",
                    r.len()
                );
                if r.len() < MIN_TAIL_SAMPLES {
                    short_tails += 1;
                }
            }
        }
        assert_eq!(short_tails, 0, "blocs sous le plancher : {}", short_tails);
    }

    #[test]
    fn ranges_never_exceed_total_samples() {
        // Masque plus long que le signal annoncé : les intervalles rendus
        // servent à indexer `samples[range]`, ils ne doivent jamais déborder.
        let total = 1_000;
        let ranges = chunk_ranges(&[false; 20_000], total);

        for r in &ranges {
            assert!(
                r.end <= total,
                "intervalle {:?} au-delà de total_samples = {}",
                r,
                total
            );
        }
    }
}
