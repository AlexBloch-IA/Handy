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

use crate::audio_toolkit::vad::{SileroVad, VoiceActivityDetector};
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{get_settings, write_settings, ModelUnloadTimeout};
use anyhow::{anyhow, Result};
use log::{error, info, warn};
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
        // Valider tout le lot d'abord : un dépôt de dix fichiers dont le
        // dernier est un .pdf ne doit pas laisser neuf jobs en file derrière
        // une erreur. Tout passe, ou rien n'est mis en file.
        for path in &paths {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();
            if !SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
                return Err(anyhow!("format non supporté: .{}", ext));
            }
        }

        let mut created = Vec::new();
        for path in paths {
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

        // Un job encore en file n'a jamais été démarré : le marquer tout de
        // suite pour que l'UI réagisse sans attendre le worker. Le verrou est
        // relâché avant d'émettre — émettre sous verrou exposerait à un
        // interblocage si un handler d'événement rappelait le manager.
        let snapshot = {
            let mut jobs = self.jobs.lock().unwrap();
            match jobs.iter_mut().find(|j| j.id == job_id) {
                Some(job) if job.status == JobStatus::Queued => {
                    job.status = JobStatus::Cancelled;
                    Some(job.clone())
                }
                _ => None,
            }
        };

        if let Some(job) = snapshot {
            self.emit(job);
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
        // chaque bloc paierait un rechargement complet. La garde restaure le
        // réglage à la sortie, quel que soit le chemin emprunté.
        let _unload_guard = UnloadTimeoutGuard::suspend(&self.app);

        let model_id = get_settings(&self.app).selected_model;
        if model_id.is_empty() {
            return Err(anyhow!(
                "aucun modèle sélectionné — choisissez-en un dans l'onglet Modèles"
            ));
        }
        self.ensure_model_loaded(&model_id)?;

        let mut pieces: Vec<String> = Vec::with_capacity(ranges.len());
        for (i, range) in ranges.iter().enumerate() {
            if self.is_cancelled(job_id) {
                self.update(job_id, |job| job.status = JobStatus::Cancelled);
                return Ok(());
            }

            // Priorité à la dictée : si l'utilisateur enregistre au micro,
            // attendre avant d'attaquer le bloc suivant. Sans ça, un fichier
            // d'une heure en fond rendrait le raccourci inutilisable, chaque
            // dictée devant attendre la fin d'un bloc de 2 min.
            self.wait_while_recording();

            // Le modèle a pu partir entre-temps (déchargement manuel, panique
            // du moteur, changement de modèle) : `transcribe()` échouerait
            // alors sec, et 40 minutes de calcul partiraient avec. Recharger
            // coûte quelques secondes, c'est toujours mieux.
            self.ensure_model_loaded(&model_id)?;

            let chunk = samples[range.clone()].to_vec();
            match self.transcription.transcribe(chunk) {
                Ok(text) => {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        pieces.push(trimmed.to_string());
                    }
                }
                Err(e) => return Err(anyhow!("bloc {}/{}: {}", i + 1, ranges.len(), e)),
            }

            self.update(job_id, |job| job.chunks_done = i + 1);
        }

        let transcript = pieces.join(" ");
        let written = output::write_transcript(&source, &transcript);
        if let Err(e) = &written {
            warn!("transcript de {} non écrit sur disque: {}", job_id, e);
        }

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

    /// Charge le modèle s'il ne l'est pas déjà. Un modèle déjà chargé — même
    /// un autre que `model_id`, l'utilisateur ayant pu changer d'avis en cours
    /// de route — est laissé tel quel.
    fn ensure_model_loaded(&self, model_id: &str) -> Result<()> {
        if self.transcription.is_model_loaded() {
            return Ok(());
        }
        self.transcription
            .load_model_with_device(model_id, None)
            .map_err(|e| anyhow!("chargement du modèle impossible: {}", e))
    }

    /// Bloque tant qu'un enregistrement micro est en cours, avec un plafond de
    /// sécurité : un état d'enregistrement resté coincé ne doit pas figer la
    /// file de fichiers pour toujours.
    fn wait_while_recording(&self) {
        use crate::managers::audio::AudioRecordingManager;

        // `try_state` et non `state` : le manager audio peut ne pas être
        // enregistré (démarrage, tests) et une panique du worker tuerait la
        // file entière.
        let Some(audio) = self.app.try_state::<Arc<AudioRecordingManager>>() else {
            return;
        };

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
}

/// Force le timeout de déchargement du modèle sur `Never` le temps d'un job et
/// restaure la valeur d'origine à la destruction.
///
/// Une closure de restauration appelée à la main serait tôt ou tard oubliée sur
/// un chemin d'erreur — et le réglage de l'utilisateur resterait modifié pour
/// de bon. `Drop` couvre tous les retours, y compris une panique du worker.
struct UnloadTimeoutGuard<'a> {
    app: &'a AppHandle,
    previous: ModelUnloadTimeout,
}

impl<'a> UnloadTimeoutGuard<'a> {
    fn suspend(app: &'a AppHandle) -> Self {
        let mut settings = get_settings(app);
        let previous = settings.model_unload_timeout;
        if previous != ModelUnloadTimeout::Never {
            settings.model_unload_timeout = ModelUnloadTimeout::Never;
            write_settings(app, settings);
        }
        Self { app, previous }
    }
}

impl Drop for UnloadTimeoutGuard<'_> {
    fn drop(&mut self) {
        if self.previous == ModelUnloadTimeout::Never {
            return;
        }
        // Relecture juste avant écriture : les réglages sont un seul objet
        // persisté d'un bloc, et écrire une copie prise il y a 40 minutes
        // effacerait tout ce que l'utilisateur a changé entre-temps.
        let mut settings = get_settings(self.app);
        if settings.model_unload_timeout == ModelUnloadTimeout::Never {
            settings.model_unload_timeout = self.previous;
            write_settings(self.app, settings);
        }
    }
}
