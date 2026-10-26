//! Music for the station (TS src/music/manager.ts with the basic and stub
//! backends): the play queue of each device lives in an external music
//! service (MUSIC_SERVICE_BASIC_URL); without one there is no music. The
//! manager remembers where playback was stopped and the commit token of the
//! station's "what plays next" request.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::directives;
use crate::proto::n_alice::n_alice_api::TDirective;

/// A playable track as the music service returns it (@v3rt3p/types/music `musicTrack`)
#[derive(Clone, Debug, Deserialize)]
pub struct MusicTrack {
    pub format: String,
    pub url: String,
}

/// What a request wants from the music subsystem (TS internal directives)
#[derive(Clone, Debug)]
pub enum MusicAction {
    /// music_play: the current track, from the position saved on stop
    Play,
    /// music_next_track
    NextTrack,
    /// music_previous_track
    PreviousTrack,
    /// music_stop: remember the playback position for the next Play
    Stop,
    /// music_recover_next_track: the station asks what to play when a track ends
    RecoverNextTrack,
    /// music_recover_commit: the station started that track, the queue moves on
    RecoverCommit(String),
}

enum Backend {
    /// TS BasicMusicBackend
    Basic { base_url: Url, client: reqwest::Client },
    /// TS StubMusicBackend: never has a track
    Stub,
}

pub struct MusicManager {
    backend: Backend,
    /// playback positions (ms) saved on stop, per device
    positions: Mutex<HashMap<String, i64>>,
    /// the outstanding recover commit token, per device
    recover_tokens: Mutex<HashMap<String, String>>,
}

impl MusicManager {
    /// The basic backend for a music service URL, the stub without one (or with an unusable one)
    pub fn new(base_url: Option<&str>) -> Self {
        let backend = match base_url.map(Url::parse) {
            None => Backend::Stub,
            Some(Err(error)) => {
                tracing::error!("MUSIC_SERVICE_BASIC_URL is not a URL ({error}): music is disabled");
                Backend::Stub
            }
            Some(Ok(base_url)) => {
                // the station waits ~15 s for an answer that may need two calls
                let client = reqwest::Client::builder()
                    .connect_timeout(Duration::from_secs(3))
                    .timeout(Duration::from_secs(5))
                    .build();
                match client {
                    Ok(client) => Backend::Basic { base_url, client },
                    Err(error) => {
                        tracing::error!("no HTTP client for the music service ({error}): music is disabled");
                        Backend::Stub
                    }
                }
            }
        };
        Self { backend, positions: Mutex::default(), recover_tokens: Mutex::default() }
    }

    pub fn is_configured(&self) -> bool {
        matches!(self.backend, Backend::Basic { .. })
    }

    /// The station directives of a music action (TS processWrappedDirectives);
    /// `offset_ms` is the station's player position, which Stop saves. A
    /// failing music service means no directives, never a failed answer
    pub async fn directives(&self, device_id: &str, action: &MusicAction, offset_ms: Option<i64>) -> Vec<TDirective> {
        match self.run(device_id, action, offset_ms).await {
            Ok(directives) => directives,
            Err(error) => {
                tracing::warn!("music {action:?} for {device_id} failed: {error:#}");
                Vec::new()
            }
        }
    }

    async fn run(&self, device_id: &str, action: &MusicAction, offset_ms: Option<i64>) -> Result<Vec<TDirective>> {
        match action {
            MusicAction::Play => {
                let position = lock(&self.positions).get(device_id).copied();
                self.play_current(device_id, position).await
            }
            MusicAction::NextTrack => {
                self.advance(device_id, "forward").await?;
                self.play_current(device_id, None).await
            }
            MusicAction::PreviousTrack => {
                self.advance(device_id, "backward").await?;
                self.play_current(device_id, None).await
            }
            MusicAction::Stop => {
                if let Some(position) = offset_ms.filter(|position| *position != 0) {
                    lock(&self.positions).insert(device_id.to_string(), position);
                }
                Ok(Vec::new())
            }
            MusicAction::RecoverNextTrack => {
                // like TS: the queue's current track, committed once the station starts it
                let Some(track) = self.current_track(device_id).await? else {
                    return Ok(Vec::new());
                };
                let token = Uuid::new_v4().to_string();
                lock(&self.recover_tokens).insert(device_id.to_string(), token.clone());
                Ok(play(&track, None, Some(&token)))
            }
            MusicAction::RecoverCommit(token) => {
                let expected = lock(&self.recover_tokens).get(device_id).cloned();
                if expected.as_deref() == Some(token.as_str()) {
                    lock(&self.recover_tokens).remove(device_id);
                    self.advance(device_id, "forward").await?;
                } else {
                    tracing::warn!(
                        "recover commit for {device_id} ignored: token {token} is not the expected {expected:?}"
                    );
                }
                Ok(Vec::new())
            }
        }
    }

    /// TS advanceQueueForward/Backward: the new track starts from its beginning
    async fn advance(&self, device_id: &str, direction: &str) -> Result<()> {
        lock(&self.positions).remove(device_id);
        let Backend::Basic { base_url, client } = &self.backend else {
            return Ok(());
        };
        let response = client.post(queue_url(base_url, device_id, direction)?).send().await?;
        // TS does not look at the status either: the current track plays anyway
        if !response.status().is_success() {
            tracing::warn!("music service answered {} to {direction} for {device_id}", response.status());
        }
        Ok(())
    }

    async fn play_current(&self, device_id: &str, offset_ms: Option<i64>) -> Result<Vec<TDirective>> {
        Ok(match self.current_track(device_id).await? {
            Some(track) => play(&track, offset_ms, None),
            None => {
                tracing::info!("no track to play for {device_id}");
                Vec::new()
            }
        })
    }

    async fn current_track(&self, device_id: &str) -> Result<Option<MusicTrack>> {
        let Backend::Basic { base_url, client } = &self.backend else {
            return Ok(None);
        };
        let response = client.get(queue_url(base_url, device_id, "current")?).send().await?;
        if !response.status().is_success() {
            bail!("music service answered {} to current", response.status());
        }
        let body: Value = response.json().await.context("music service answered with invalid JSON")?;
        if body.is_null() {
            return Ok(None);
        }
        let track: MusicTrack = serde_json::from_value(body).context("music service answered with no track")?;
        Url::parse(&track.url).with_context(|| format!("track URL '{}' is invalid", track.url))?;
        Ok(Some(track))
    }
}

/// `{base}/queue/{device}/{operation}`, the device id escaped as a path segment
fn queue_url(base_url: &Url, device_id: &str, operation: &str) -> Result<Url> {
    let mut url = base_url.clone();
    url.path_segments_mut()
        .map_err(|()| anyhow::anyhow!("MUSIC_SERVICE_BASIC_URL cannot have a path"))?
        .pop_if_empty()
        .extend(["queue", device_id, operation]);
    Ok(url)
}

/// audio_play of the track plus the request for the following one (TS)
fn play(track: &MusicTrack, offset_ms: Option<i64>, recover_commit_token: Option<&str>) -> Vec<TDirective> {
    vec![directives::audio_play(track, offset_ms, recover_commit_token), directives::stack_recover_next_track()]
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
