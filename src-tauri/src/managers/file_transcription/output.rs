//! Choix du chemin de sortie du transcript et écriture sur disque.

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
