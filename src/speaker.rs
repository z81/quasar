//! Household voice personalization. Only embeddings are persisted; audio is
//! passed to an explicitly configured local encoder and discarded after use.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::storage::Storage;

const KEY: &str = "speaker_profiles:v1";
const MODEL: &str = "speechbrain-ecapa-0f99f2d0";
const DIMENSIONS: usize = 192;
const SAMPLES: usize = 3;
const MAX_PROFILES: usize = 20;
// Conservative starting points, not calibrated probabilities or authentication.
const MATCH_THRESHOLD: f32 = 0.65;
const MATCH_MARGIN: f32 = 0.12;
const ENROLL_CONSISTENCY: f32 = 0.65;

#[derive(Clone, Serialize, Deserialize)]
struct Profile {
    id: String,
    name: String,
    preferences: String,
    model: String,
    embeddings: Vec<Vec<f32>>,
}

#[derive(Clone, Serialize)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub preferences: String,
}

impl Profile {
    fn identity(&self) -> Identity {
        Identity {
            id: self.id.clone(),
            name: self.name.clone(),
            preferences: self.preferences.clone(),
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Document {
    #[serde(default)]
    profiles: Vec<Profile>,
}

struct Enrollment {
    id: String,
    device: String,
    name: String,
    preferences: String,
    expires: Instant,
    embeddings: Vec<Vec<f32>>,
    message: String,
}

#[derive(Clone, Serialize)]
pub struct Decision {
    pub status: &'static str,
    pub name: Option<String>,
    pub message: String,
}

#[derive(Serialize)]
pub struct EnrollmentView {
    pub device: String,
    pub name: String,
    pub samples: usize,
    pub required: usize,
    pub seconds_remaining: u64,
    pub message: String,
}

#[derive(Serialize)]
pub struct View {
    pub persistent: bool,
    pub backend: &'static str,
    pub message: String,
    pub profiles: Vec<Identity>,
    pub enrollment: Option<EnrollmentView>,
    pub last_decision: Option<Decision>,
}

struct State {
    document: Document,
    enrollment: Option<Enrollment>,
    last: HashMap<String, Decision>,
}

pub struct SpeakerService {
    storage: Arc<Storage>,
    client: reqwest::Client,
    state: Mutex<State>,
    // A synchronous snapshot lets VoiceInput capture consent before recording
    // and avoid starting STT or external context requests during enrollment.
    active: RwLock<Option<(String, String, Instant)>>,
}

pub enum Outcome {
    Identity(Option<Identity>),
    Enrollment(String),
}

#[derive(Deserialize)]
struct Embedding {
    model: String,
    embedding: Vec<f32>,
    speech_seconds: f32,
}

impl SpeakerService {
    pub async fn load(storage: Arc<Storage>) -> Result<Arc<Self>> {
        let document: Document = storage
            .get_control_value(KEY)
            .await?
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        ensure!(
            document.profiles.len() <= MAX_PROFILES,
            "Слишком много голосовых профилей"
        );
        Ok(Arc::new(Self {
            storage,
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(1))
                .timeout(Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            state: Mutex::new(State {
                document,
                enrollment: None,
                last: HashMap::new(),
            }),
            active: RwLock::new(None),
        }))
    }

    pub async fn view(&self, endpoint: Option<&str>, device: &str) -> View {
        let (backend, message) = match endpoint {
            None => (
                "disabled",
                "Укажите адрес локального сервиса в настройках сервера.",
            ),
            Some(endpoint) => {
                let response = self
                    .client
                    .get(format!("{}/health", endpoint.trim_end_matches('/')))
                    .timeout(Duration::from_secs(2))
                    .send()
                    .await;
                let ready = match response {
                    Ok(response) if response.status().is_success() => bounded_body(response)
                        .await
                        .and_then(|body| serde_json::from_slice::<serde_json::Value>(&body).ok())
                        .is_some_and(|body| body["model"] == MODEL && body["status"] == "ready"),
                    _ => false,
                };
                if ready {
                    (
                        "ready",
                        "Локальный сервис доступен. Голос используется только для персонализации.",
                    )
                } else {
                    ("unavailable", "Сервис распознавания недоступен или несовместим. Алиса отвечает без личного профиля.")
                }
            }
        };
        let mut state = self.state.lock().await;
        expire(&mut state);
        View {
            persistent: self.storage.is_persistent(),
            backend,
            message: message.into(),
            profiles: state
                .document
                .profiles
                .iter()
                .map(Profile::identity)
                .collect(),
            enrollment: state.enrollment.as_ref().map(|pending| EnrollmentView {
                device: pending.device.clone(),
                name: pending.name.clone(),
                samples: pending.embeddings.len(),
                required: SAMPLES,
                seconds_remaining: pending
                    .expires
                    .saturating_duration_since(Instant::now())
                    .as_secs(),
                message: pending.message.clone(),
            }),
            last_decision: state.last.get(device).cloned(),
        }
    }

    pub async fn begin(&self, device: String, name: String, preferences: String) -> Result<()> {
        let (name, preferences) = validate_fields(name, preferences)?;
        let mut state = self.state.lock().await;
        expire(&mut state);
        ensure!(
            state.enrollment.is_none(),
            "Обучение уже идёт. Завершите или отмените его."
        );
        ensure!(
            state.document.profiles.len() < MAX_PROFILES,
            "Допустимо не больше 20 профилей"
        );
        ensure!(
            !state
                .document
                .profiles
                .iter()
                .any(|p| p.name.to_lowercase() == name.to_lowercase()),
            "Профиль с таким именем уже есть"
        );
        let pending = Enrollment {
            id: Uuid::new_v4().to_string(), device, name, preferences,
            expires: Instant::now() + Duration::from_secs(120), embeddings: Vec::new(),
            message: "Трижды обратитесь к Алисе. В каждой фразе говорите один, не меньше трёх секунд, разными словами.".into(),
        };
        *self.active.write().unwrap_or_else(PoisonError::into_inner) =
            Some((pending.device.clone(), pending.id.clone(), pending.expires));
        state.enrollment = Some(pending);
        Ok(())
    }

    pub fn enrollment_token(&self, device: &str) -> Option<String> {
        self.active
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .filter(|(duid, _, expires)| duid == device && *expires > Instant::now())
            .map(|(_, id, _)| id.clone())
    }

    pub async fn cancel(&self) {
        let mut state = self.state.lock().await;
        state.enrollment = None;
        *self.active.write().unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        let mut document = state.document.clone();
        ensure!(
            document.profiles.iter().any(|p| p.id == id),
            "Профиль не найден"
        );
        document.profiles.retain(|p| p.id != id);
        self.storage
            .set_control_value(KEY, &serde_json::to_value(&document)?)
            .await?;
        state.document = document;
        state.last.clear();
        Ok(())
    }

    pub async fn update(&self, id: &str, name: String, preferences: String) -> Result<()> {
        let (name, preferences) = validate_fields(name, preferences)?;
        let mut state = self.state.lock().await;
        ensure!(
            !state
                .document
                .profiles
                .iter()
                .any(|p| p.id != id && p.name.to_lowercase() == name.to_lowercase()),
            "Профиль с таким именем уже есть"
        );
        let mut document = state.document.clone();
        let profile = document
            .profiles
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| anyhow::anyhow!("Профиль не найден"))?;
        profile.name = name;
        profile.preferences = preferences;
        self.storage
            .set_control_value(KEY, &serde_json::to_value(&document)?)
            .await?;
        state.document = document;
        Ok(())
    }

    pub async fn process(
        &self,
        endpoint: Option<&str>,
        device: &str,
        ogg: &[u8],
        enrollment_token: Option<&str>,
    ) -> Outcome {
        {
            let mut state = self.state.lock().await;
            expire(&mut state);
            if let Some(id) = enrollment_token {
                if state
                    .enrollment
                    .as_ref()
                    .is_none_or(|p| p.id != id || p.device != device)
                {
                    return Outcome::Enrollment(
                        "Обучение завершено или отменено. Образец не сохранён.".into(),
                    );
                }
            } else if state.document.profiles.is_empty() {
                return Outcome::Identity(None);
            }
        }
        let encoded = self.encode(endpoint, ogg).await;
        let mut state = self.state.lock().await;
        expire(&mut state);
        // A cancel/new enrollment during inference must never consume this sample.
        if let Some(id) = enrollment_token {
            if state.enrollment.as_ref().is_none_or(|p| p.id != id) {
                return Outcome::Enrollment(
                    "Обучение завершено или отменено. Образец не сохранён.".into(),
                );
            }
            let result = match encoded {
                Ok(sample) if sample.speech_seconds >= 3.0 => sample,
                Ok(_) => {
                    return enrollment_message(
                        &mut state,
                        "Образец слишком короткий. Говорите не меньше трёх секунд.",
                    )
                }
                Err(message) => return enrollment_message(&mut state, &message),
            };
            let pending = state.enrollment.as_ref().unwrap();
            if pending
                .embeddings
                .iter()
                .any(|sample| cosine(sample, &result.embedding) < ENROLL_CONSISTENCY)
            {
                return enrollment_message(
                    &mut state,
                    "Голос отличается от предыдущего образца. Повторите в тишине тем же голосом.",
                );
            }
            if state
                .document
                .profiles
                .iter()
                .any(|profile| score(profile, &result.embedding) >= MATCH_THRESHOLD)
            {
                return enrollment_message(&mut state, "Этот голос похож на существующий профиль. Отмените обучение и проверьте профили.");
            }
            let pending = state.enrollment.as_mut().unwrap();
            pending.embeddings.push(result.embedding);
            if pending.embeddings.len() < SAMPLES {
                let message = format!(
                    "Образец {} из {} принят. Скажите ещё одну фразу не короче трёх секунд.",
                    pending.embeddings.len(),
                    SAMPLES
                );
                return enrollment_message(&mut state, &message);
            }
            let profile = Profile {
                id: pending.id.clone(),
                name: pending.name.clone(),
                preferences: pending.preferences.clone(),
                model: MODEL.into(),
                embeddings: pending.embeddings.clone(),
            };
            let mut document = state.document.clone();
            document.profiles.push(profile);
            if self
                .storage
                .set_control_value(KEY, &serde_json::to_value(&document).unwrap())
                .await
                .is_err()
            {
                state.enrollment = None;
                *self.active.write().unwrap_or_else(PoisonError::into_inner) = None;
                return Outcome::Enrollment(
                    "Не удалось сохранить профиль. Обучение отменено; попробуйте позже.".into(),
                );
            }
            state.document = document;
            state.enrollment = None;
            *self.active.write().unwrap_or_else(PoisonError::into_inner) = None;
            return Outcome::Enrollment(
                "Голосовой профиль сохранён. Теперь можно обращаться к Алисе.".into(),
            );
        }
        let (identity, status, message) = match encoded {
            Err(message) => (None, "unknown", message),
            Ok(sample) if sample.speech_seconds < 2.0 => (
                None,
                "unknown",
                "Слишком мало речи для уверенного распознавания.".into(),
            ),
            Ok(sample) => {
                let mut ranked: Vec<_> = state
                    .document
                    .profiles
                    .iter()
                    .map(|p| (score(p, &sample.embedding), p))
                    .collect();
                ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
                match ranked.first() {
                    Some((best, profile))
                        if *best >= MATCH_THRESHOLD
                            && ranked
                                .get(1)
                                .is_none_or(|next| *best - next.0 >= MATCH_MARGIN) =>
                    {
                        (
                            Some(profile.identity()),
                            "recognized",
                            "Голос сопоставлен с профилем. Это не проверка личности для доступа."
                                .into(),
                        )
                    }
                    _ => (
                        None,
                        "unknown",
                        "Нет уверенного совпадения. Используется общий профиль.".into(),
                    ),
                }
            }
        };
        if state.last.len() >= 100 && !state.last.contains_key(device) {
            state.last.clear();
        }
        state.last.insert(
            device.into(),
            Decision {
                status,
                name: identity.as_ref().map(|p| p.name.clone()),
                message,
            },
        );
        Outcome::Identity(identity)
    }

    async fn encode(
        &self,
        endpoint: Option<&str>,
        ogg: &[u8],
    ) -> std::result::Result<Embedding, String> {
        let Some(endpoint) = endpoint else {
            return Err("Распознавание голосов отключено.".into());
        };
        if ogg.is_empty() || ogg.len() > 8 * 1024 * 1024 {
            return Err("Неподходящий размер голосового образца.".into());
        }
        let response = self
            .client
            .post(format!("{}/embedding", endpoint.trim_end_matches('/')))
            .header("Content-Type", "audio/ogg")
            .body(ogg.to_vec())
            .send()
            .await
            .map_err(|_| "Сервис распознавания недоступен. Образец не сохранён.")?;
        if !response.status().is_success() {
            return Err("Не удалось выделить голос. Повторите в тишине, без музыки.".into());
        }
        let bytes = bounded_body(response)
            .await
            .ok_or("Некорректный ответ сервиса голосов.")?;
        let mut result: Embedding =
            serde_json::from_slice(&bytes).map_err(|_| "Некорректный ответ сервиса голосов.")?;
        if result.model != MODEL
            || result.embedding.len() != DIMENSIONS
            || !result.speech_seconds.is_finite()
            || !(0.0..=30.0).contains(&result.speech_seconds)
            || result.embedding.iter().any(|n| !n.is_finite())
        {
            return Err(
                "Несовместимый голосовой отпечаток. Проверьте версию локального сервиса.".into(),
            );
        }
        let norm = result.embedding.iter().map(|n| n * n).sum::<f32>().sqrt();
        if !norm.is_finite() || norm < 0.001 {
            return Err("Пустой голосовой отпечаток.".into());
        }
        for value in &mut result.embedding {
            *value /= norm;
        }
        Ok(result)
    }
}

async fn bounded_body(mut response: reqwest::Response) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    if response
        .content_length()
        .is_some_and(|size| size > 32 * 1024)
    {
        return None;
    }
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > 32 * 1024 {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

fn validate_fields(name: String, preferences: String) -> Result<(String, String)> {
    let name = name.trim().to_owned();
    let preferences = preferences.trim().to_owned();
    ensure!(
        !name.is_empty() && name.chars().count() <= 80 && !name.chars().any(char::is_control),
        "Имя должно содержать от 1 до 80 символов"
    );
    if preferences.chars().count() > 2000 {
        bail!("Предпочтения должны быть не длиннее 2000 символов");
    }
    Ok((name, preferences))
}

fn expire(state: &mut State) {
    if state
        .enrollment
        .as_ref()
        .is_some_and(|p| p.expires <= Instant::now())
    {
        state.enrollment = None;
    }
}

fn enrollment_message(state: &mut State, message: &str) -> Outcome {
    if let Some(pending) = state.enrollment.as_mut() {
        pending.message = message.into();
    }
    Outcome::Enrollment(message.into())
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return -1.0;
    }
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}

fn score(profile: &Profile, sample: &[f32]) -> f32 {
    if profile.model != MODEL || profile.embeddings.len() != SAMPLES {
        return -1.0;
    }
    profile
        .embeddings
        .iter()
        .map(|reference| cosine(reference, sample))
        .sum::<f32>()
        / SAMPLES as f32
}
