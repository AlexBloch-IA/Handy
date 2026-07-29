//! Décodage d'un fichier audio arbitraire vers le format qu'attend le moteur
//! de transcription : PCM f32, mono, 16 kHz.

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
        .ok_or_else(|| anyhow!("fréquence d'échantillonnage inconnue"))?
        as usize;
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

    let mut resampler =
        FrameResampler::new(source_rate, TARGET_SAMPLE_RATE, Duration::from_millis(30));
    let mut out: Vec<f32> = Vec::with_capacity(mono.len() * TARGET_SAMPLE_RATE / source_rate + 1);
    resampler.push(&mono, |frame| out.extend_from_slice(frame));
    resampler.finish(|frame| out.extend_from_slice(frame));

    Ok(out)
}

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

        assert!(
            result.is_err(),
            "un fichier corrompu doit remonter une erreur"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
