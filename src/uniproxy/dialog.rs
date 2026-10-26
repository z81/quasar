//! Dialog turns: a voice input or an external event goes through the agent and
//! the answer goes out as AliceResponse(s) + streamed TTS. Turns run in tasks
//! spawned by the connection; the connection's current dialog (its id and
//! agent session) lives in a `DialogSlot` shared with them.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde_json::Value;
use uuid::Uuid;

use super::UniproxyContext;
use super::metadata::{BiometrySlot, InputMetadata};
use super::outbox::{Outbox, ResponseTarget};
use crate::agent::protocol::FunctionCall;
use crate::agent::{Agent, AgentOutput, AgentTurn};
use crate::audio;
use crate::directives;
use crate::prompt::system_state;
use crate::proto::n_alice::n_alice_api::TDirective;

/// Spoken when the LLM fails, instead of leaving the station silent (as in TS)
const ERROR_RESPONSE_TEXT: &str = "Извините, что-то пошло не так. Попробуйте спросить ещё раз.";

struct Dialog {
    /// Server-assigned (TS: a UUID per dialog): answers carry it, and the
    /// station names it in requests that continue the dialog
    id: String,
    /// The LLM session: history plus the session id for remote servers
    agent: Arc<Agent>,
    /// The last answer asked for more input, so the next voice input continues
    awaiting_input: bool,
}

/// The connection's current dialog, shared by the receive loop and the turns
#[derive(Clone, Default)]
pub struct DialogSlot(Arc<Mutex<Option<Dialog>>>);

impl DialogSlot {
    fn lock(&self) -> MutexGuard<'_, Option<Dialog>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The dialog of a new voice input: the current one if its last answer
    /// asked for more input and the station does not name another dialog,
    /// otherwise a new one (TS interrupts the old dialog)
    pub fn for_voice_input(&self, context: &UniproxyContext, station_dialog_id: Option<&str>) -> (String, Arc<Agent>) {
        let mut slot = self.lock();
        if let Some(dialog) = slot.as_mut() {
            let names_other = station_dialog_id.is_some_and(|id| !id.is_empty() && id != dialog.id);
            if dialog.awaiting_input && !names_other {
                dialog.awaiting_input = false;
                return (dialog.id.clone(), dialog.agent.clone());
            }
        }
        open(&mut slot, context)
    }

    /// The dialog of an external event: like TS, the event joins the current
    /// agent session, under a new dialog id unless the request names the
    /// current one
    pub fn for_event(&self, context: &UniproxyContext, station_dialog_id: Option<&str>) -> (String, Arc<Agent>) {
        let mut slot = self.lock();
        match slot.as_mut() {
            Some(dialog) => {
                if station_dialog_id != Some(dialog.id.as_str()) {
                    dialog.id = Uuid::new_v4().to_string();
                }
                dialog.awaiting_input = false;
                (dialog.id.clone(), dialog.agent.clone())
            }
            None => open(&mut slot, context),
        }
    }

    /// The dialog id for an answer that does not go through the agent: the
    /// current dialog's if the request names it, a fresh one otherwise (as TS)
    pub fn response_dialog_id(&self, station_dialog_id: Option<&str>) -> String {
        match (self.lock().as_ref(), station_dialog_id) {
            (Some(dialog), Some(id)) if dialog.id == id => dialog.id.clone(),
            _ => Uuid::new_v4().to_string(),
        }
    }

    pub fn current_id(&self) -> Option<String> {
        self.lock().as_ref().map(|dialog| dialog.id.clone())
    }

    /// A turn of dialog `id` was answered: keep the dialog for the follow-up
    /// the answer asked for, or end it (the next request starts a fresh history)
    fn finish_turn(&self, id: &str, should_listen: bool) {
        let mut slot = self.lock();
        match slot.as_mut() {
            Some(dialog) if dialog.id == id && should_listen => dialog.awaiting_input = true,
            Some(dialog) if dialog.id == id => {
                tracing::info!("dialog {id} finished");
                *slot = None;
            }
            // a newer dialog took over meanwhile
            _ => {}
        }
    }

    pub fn close(&self) {
        if let Some(dialog) = self.lock().take() {
            tracing::info!("dialog {} closed", dialog.id);
        }
    }
}

fn open(slot: &mut Option<Dialog>, context: &UniproxyContext) -> (String, Arc<Agent>) {
    let dialog = Dialog { id: Uuid::new_v4().to_string(), agent: Arc::new(new_agent(context)), awaiting_input: false };
    tracing::info!("dialog {} started", dialog.id);
    let opened = (dialog.id.clone(), dialog.agent.clone());
    *slot = Some(dialog);
    opened
}

fn new_agent(context: &UniproxyContext) -> Agent {
    let system_prompt = context.prompt_generator.generate(&context.functions).unwrap_or_else(|error| {
        tracing::error!("failed to render the system prompt: {error:#}");
        String::new()
    });
    let state_prompt = context.prompt_generator.generate_state(&system_state()).unwrap_or_else(|error| {
        tracing::error!("failed to render the state prompt: {error:#}");
        String::new()
    });
    Agent::new(
        context.openrouter.clone(),
        context.config.processor_model.clone(),
        context.functions.clone(),
        system_prompt,
        state_prompt,
        context.cache.clone(),
    )
}

/// The audio of a finished VoiceInput with what came along with it
pub struct Utterance {
    pub ogg: Vec<u8>,
    pub device_state: Option<Vec<u8>>,
    pub biometry: BiometrySlot,
}

/// One request answered within a dialog
pub struct Turn {
    pub context: Arc<UniproxyContext>,
    pub outbox: Outbox,
    pub dialog: DialogSlot,
    pub agent: Arc<Agent>,
    pub target: ResponseTarget,
}

impl Turn {
    /// VoiceInput: transcribe, report the recognition, answer through the agent
    pub async fn answer_voice(self, utterance: Utterance) {
        let text = transcribe(&self.context, &utterance.ogg).await;
        tracing::info!("transcribed: '{text}'");
        if let Err(error) = self.outbox.asr_result(&text, &self.target.reference_message_id) {
            tracing::warn!("{error:#}");
            return;
        }

        if text.is_empty() {
            // nothing recognized: close the request with an empty answer so the station does not wait for one
            if let Err(error) = self.outbox.empty_response(&self.target) {
                tracing::warn!("{error:#}");
            }
            self.dialog.finish_turn(&self.target.dialog_id, false);
            return;
        }
        let metadata = InputMetadata::new(utterance.device_state.as_deref(), utterance.biometry.get());
        self.run_agent(AgentTurn { external_event: false, text }, &metadata).await;
    }

    /// TextInput with an external event pushed through the API
    pub async fn answer_event(self, event: String, device_state: Option<Vec<u8>>) {
        let metadata = InputMetadata::new(device_state.as_deref(), None);
        self.run_agent(AgentTurn { external_event: true, text: event }, &metadata).await;
    }

    /// Runs the agent (apologizing instead of going silent when it fails) and sends the answer
    async fn run_agent(&self, turn: AgentTurn, metadata: &InputMetadata) {
        let metadata_json = metadata.to_json();
        let session_id = self.agent.session_id().to_string();
        let state = dialog_state(&self.context, &session_id, metadata, &metadata_json).await;
        let state_prompt = self.context.prompt_generator.generate_state(&state).unwrap_or_else(|error| {
            tracing::error!("failed to render the state prompt: {error:#}");
            String::new()
        });

        let executor = remote_executor(&self.context, session_id, metadata_json);
        let outputs = match self.agent.run_turn(turn, state_prompt, executor).await {
            Ok(outputs) => outputs,
            Err(error) => {
                tracing::error!("agent turn failed: {error:#}");
                vec![AgentOutput {
                    directives: Vec::new(),
                    finished: true,
                    should_listen: false,
                    text: ERROR_RESPONSE_TEXT.to_string(),
                }]
            }
        };

        self.send_answer(outputs).await;
    }

    /// AliceResponse with the station directives + streamed TTS, per agent
    /// output. The last response settles the dialog before it is spoken (as
    /// TS closes its session first), so a voice input barging in while the
    /// answer plays continues the dialog only if the answer asked for that
    async fn send_answer(&self, outputs: Vec<AgentOutput>) {
        let count = outputs.len();
        if count == 0 {
            self.dialog.finish_turn(&self.target.dialog_id, false);
        }
        for (index, output) in outputs.into_iter().enumerate() {
            let mut station_directives: Vec<TDirective> =
                output.directives.iter().filter_map(directives::to_station_directive).collect();
            if !output.text.is_empty() {
                station_directives.push(directives::tts_play_placeholder());
            }
            let sent = self.outbox.alice_response(&self.target, &output.text, output.should_listen, station_directives);
            if let Err(error) = sent {
                tracing::warn!("answer not delivered: {error:#}");
                self.dialog.finish_turn(&self.target.dialog_id, false);
                return;
            }
            if index + 1 == count {
                self.dialog.finish_turn(&self.target.dialog_id, output.should_listen);
            }
            speak(&self.context, &self.outbox, &output.text, &self.target.reference_message_id).await;
        }
    }
}

/// TextInput with text pushed through the API: spoken as is
pub async fn answer_tts(context: Arc<UniproxyContext>, outbox: Outbox, target: ResponseTarget, text: String) {
    match outbox.alice_response(&target, &text, false, vec![directives::tts_play_placeholder()]) {
        Ok(()) => speak(&context, &outbox, &text, &target.reference_message_id).await,
        Err(error) => tracing::warn!("{error:#}"),
    }
}

/// Transcribes the whole utterance; failures count as "nothing heard"
async fn transcribe(context: &UniproxyContext, ogg: &[u8]) -> String {
    if ogg.is_empty() {
        return String::new();
    }
    let wav = match audio::ogg_to_wav(ogg).await {
        Ok(wav) => wav,
        Err(error) => {
            tracing::warn!("transcoding the utterance failed: {error:#}");
            return String::new();
        }
    };
    match context.openrouter.transcribe(&context.config.stt_model, context.config.stt_language.as_deref(), &wav).await {
        Ok(text) => text.trim().to_string(),
        Err(error) => {
            tracing::warn!("transcription failed: {error:#}");
            String::new()
        }
    }
}

/// Synthesizes `text` and streams the audio to the station as one
/// LazyTtsStreaming OGG/Opus stream; empty text is not spoken at all
async fn speak(context: &UniproxyContext, outbox: &Outbox, text: &str, reference_message_id: &str) {
    if text.is_empty() {
        return;
    }
    if let Err(error) = speak_inner(context, outbox, text, reference_message_id).await {
        tracing::error!("TTS streaming failed: {error:#}");
    }
}

async fn speak_inner(context: &UniproxyContext, outbox: &Outbox, text: &str, reference_message_id: &str) -> Result<()> {
    // request synthesis first: if it fails, no stream gets announced to the station
    let pcm = context
        .openrouter
        .speech_stream(
            &context.config.tts_model,
            &context.config.tts_voice,
            &context.config.tts_instructions,
            context.config.tts_speed,
            text,
        )
        .await
        .context("speech synthesis request failed")?;

    // once announced, the stream is closed in any case (end, error, aborted turn) by the guard
    let stream = outbox.open_tts_stream(reference_message_id)?;
    let ogg_stream = audio::pcm_stream_to_ogg(pcm, context.config.tts_pcm_sample_rate);
    tokio::pin!(ogg_stream);
    while let Some(chunk) = ogg_stream.next().await {
        stream.send_audio(&chunk.context("tts transcode failed")?)?;
    }
    Ok(())
}

/// The per-turn state shown to the LLM: system facts, the speaker, remote state
/// servers (system entries win over remote duplicates, as in TS)
async fn dialog_state(
    context: &UniproxyContext,
    session_id: &str,
    metadata: &InputMetadata,
    metadata_json: &Value,
) -> BTreeMap<String, String> {
    let mut state = system_state();
    let (age, gender) = metadata.biometry.map_or(("unknown", "unknown"), |biometry| (biometry.age, biometry.gender));
    state.insert("input_person_age".to_string(), age.to_string());
    state.insert("input_person_gender".to_string(), gender.to_string());
    for (key, (_description, value)) in context.remote.state(session_id, metadata_json).await {
        state.entry(key).or_insert(value);
    }
    state
}

/// Runs remote functions in detached tasks: neither the answer, nor an
/// interrupted dialog, nor a closed connection waits for or cancels a call
/// the LLM decided on; a scheduled call ("через час") sleeps in its task
fn remote_executor(
    context: &UniproxyContext,
    session_id: String,
    metadata: Value,
) -> impl Fn(FunctionCall) -> Result<()> + Send + Sync + 'static {
    let remote = context.remote.clone();
    move |call| {
        let (remote, session_id, metadata) = (remote.clone(), session_id.clone(), metadata.clone());
        tokio::spawn(async move {
            if let Some(delay_ms) = call.schedule_ms.filter(|milliseconds| *milliseconds > 0) {
                tracing::info!("calling {} after {delay_ms}ms", call.name);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            if let Err(error) = remote.call_by_name(&call.name, &call.arguments, &session_id, &metadata).await {
                tracing::warn!("remote function '{}' failed: {error:#}", call.name);
            }
        });
        Ok(())
    }
}
