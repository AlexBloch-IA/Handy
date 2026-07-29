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
pub fn cancel_file_transcription(manager: State<Arc<FileTranscriptionManager>>, job_id: String) {
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
