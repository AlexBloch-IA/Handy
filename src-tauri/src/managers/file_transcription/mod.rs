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
