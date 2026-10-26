//! Dialogs and how requests are answered. A voice input or an external event
//! runs an agent turn whose answer parts (one per agent iteration) queue up;
//! every request of the station takes one part, as the TS InputHandler does:
//! the first part answers the request itself, later parts answer the
//! station's "continue" requests. A part that is not ready within 5 s is
//! replaced by a continue stage 1 server_action, so the station never waits
//! long for a response and comes back for the answer (stage 1 -> push of
//! stage 2 -> stage 2 request -> the part, or another 5 s wait).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::metadata::{self, BiometrySlot, InputMetadata};
use super::outbox::{Outbox, ResponseTarget};
use super::{UniproxyContext, report};
use crate::agent::protocol::FunctionCall;
use crate::agent::{Agent, AgentOutput, AgentTurn, FunctionExecutor};
use crate::audio;
use crate::directives::{self, WrappedDirective};
use crate::prompt::system_state;
use crate::proto::n_alice::n_alice_api::TDirective;
use crate::remote::RemoteServers;

/// Spoken when the LLM fails, instead of leaving the station silent (as in TS)
const ERROR_RESPONSE_TEXT: &str = "Извините, что-то пошло не так. Попробуйте спросить ещё раз.";

/// How long a request waits for an answer part before the station is asked
/// to come back for it (TS MAX_WAIT)
const PART_WAIT: Duration = Duration::from_secs(5);

/// The answer parts of one turn that the station has not taken yet (TS partialResponses)
type Answers = Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<AgentOutput>>>;

struct Dialog {
    /// Server-assigned (TS: a UUID per dialog): answers carry it, and the
    /// station names it in requests that continue the dialog
    id: String,
    /// The LLM session: history plus the session id for remote servers
    agent: Arc<Agent>,
    /// The last answer asked for more input, so the next voice input continues
    awaiting_input: bool,
    /// The parts of the dialog's latest turn, for continue requests
    answers: Option<Answers>,
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

    /// Continue requests of dialog `id` take their parts from this turn
    fn set_answers(&self, id: &str, answers: Answers) {
        if let Some(dialog) = self.lock().as_mut().filter(|dialog| dialog.id == id) {
            dialog.answers = Some(answers);
        }
    }

    /// The pending parts of dialog `id`; none once the dialog was replaced or closed
    fn answers(&self, id: &str) -> Option<Answers> {
        self.lock().as_ref().filter(|dialog| dialog.id == id).and_then(|dialog| dialog.answers.clone())
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
    let dialog = Dialog {
        id: Uuid::new_v4().to_string(),
        agent: Arc::new(new_agent(context)),
        awaiting_input: false,
        answers: None,
    };
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

/// The audio of a finished VoiceInput and the speaker it came from
pub struct Utterance {
    pub ogg: Vec<u8>,
    pub biometry: BiometrySlot,
}

/// A request of the station and what answering it needs
pub struct Request {
    pub context: Arc<UniproxyContext>,
    pub outbox: Outbox,
    pub dialog: DialogSlot,
    pub device_id: Arc<str>,
    pub target: ResponseTarget,
    /// The request's TDeviceState: metadata for remote servers, the player
    /// position a music stop saves
    pub device_state: Option<Vec<u8>>,
}

impl Request {
    /// VoiceInput: transcribe, report the recognition, answer through the agent
    pub async fn answer_voice(self, agent: Arc<Agent>, utterance: Utterance) {
        let text = transcribe(&self.context, &utterance.ogg).await;
        tracing::info!("transcribed: '{text}'");
        if let Err(error) = self.outbox.asr_result(&text, &self.target.reference_message_id) {
            tracing::warn!("{error:#}");
            return;
        }
        if text.is_empty() {
            // nothing recognized: close the request with an empty answer so the station does not wait for one
            report(self.outbox.empty_response(&self.target));
            self.dialog.finish_turn(&self.target.dialog_id, false);
            return;
        }
        // MatchedUser may arrive while the utterance is transcribed, so biometry is read only now (as in TS)
        let metadata = InputMetadata::new(self.device_state.as_deref(), utterance.biometry.get());
        self.answer_with_agent(&agent, AgentTurn { external_event: false, text }, &metadata).await;
    }

    /// TextInput with an external event pushed through the API
    pub async fn answer_event(self, agent: Arc<Agent>, event: String) {
        let metadata = InputMetadata::new(self.device_state.as_deref(), None);
        self.answer_with_agent(&agent, AgentTurn { external_event: true, text: event }, &metadata).await;
    }

    /// Continue stage 2: the next part of the dialog's answer
    pub async fn answer_continue(self) {
        match self.dialog.answers(&self.target.dialog_id) {
            Some(answers) => self.answer_next(&answers).await,
            // no turn of that dialog is pending (it ended, or a newer dialog took over)
            None => report(self.outbox.empty_response(&self.target)),
        }
    }

    /// TextInput with text pushed through the API: spoken as is
    pub async fn answer_tts(self, text: String) {
        match self.outbox.alice_response(&self.target, &text, false, vec![directives::tts_play_placeholder(None)]) {
            Ok(()) => speak(&self.context, &self.outbox, &text, &self.target.reference_message_id).await,
            Err(error) => tracing::warn!("{error:#}"),
        }
    }

    /// Text commands and music requests: directives without speech
    pub async fn answer_directives(self, wrapped: Vec<WrappedDirective>) {
        let directives = self.resolve(wrapped).await;
        report(self.outbox.alice_response(&self.target, "", false, directives));
    }

    /// Runs the agent turn while this request waits for its first part; the
    /// later parts wait for continue requests
    async fn answer_with_agent(&self, agent: &Arc<Agent>, turn: AgentTurn, metadata: &InputMetadata) {
        let (parts, answers) = mpsc::unbounded_channel();
        let answers: Answers = Arc::new(tokio::sync::Mutex::new(answers));
        self.dialog.set_answers(&self.target.dialog_id, answers.clone());
        tokio::join!(self.run_agent(agent, turn, metadata, parts), self.answer_next(&answers));
    }

    async fn run_agent(
        &self,
        agent: &Arc<Agent>,
        turn: AgentTurn,
        metadata: &InputMetadata,
        parts: mpsc::UnboundedSender<AgentOutput>,
    ) {
        let metadata_json = metadata.to_json();
        let session_id = agent.session_id().to_string();
        let state = dialog_state(&self.context, &session_id, metadata, &metadata_json).await;
        let state_prompt = self.context.prompt_generator.generate_state(&state).unwrap_or_else(|error| {
            tracing::error!("failed to render the state prompt: {error:#}");
            String::new()
        });
        let executor = RemoteExecutor { remote: self.context.remote.clone(), session_id, metadata: metadata_json };
        if let Err(error) = agent.run_turn(turn, state_prompt, &executor, &parts).await {
            tracing::error!("agent turn failed: {error:#}");
            // like TS: an apology instead of silence, as the turn's last part
            let _ = parts.send(AgentOutput {
                directives: Vec::new(),
                finished: true,
                should_listen: false,
                text: ERROR_RESPONSE_TEXT.to_string(),
            });
        }
    }

    /// Answers with the turn's next part, waiting up to 5 s for it (TS getPartialResponseResult)
    async fn answer_next(&self, answers: &Answers) {
        let next = {
            let mut answers = answers.lock().await;
            tokio::time::timeout(PART_WAIT, answers.recv()).await
        };
        match next {
            Ok(Some(part)) => self.deliver(part).await,
            // the turn has nothing more to say
            Ok(None) => report(self.outbox.empty_response(&self.target)),
            Err(_) => {
                tracing::info!("no answer within {PART_WAIT:?}: the station is asked to continue");
                let stage1 = directives::continue_session_stage1_frame(&self.target.dialog_id);
                let directives = vec![directives::mm_semantic_frame(&stage1)];
                report(self.outbox.alice_response(&self.target, "", false, directives));
            }
        }
    }

    /// AliceResponse with the part's directives and TTS. A part that is not
    /// the last makes the station come back for the next one once it is
    /// spoken (or at once when there is nothing to say); the last part
    /// settles the dialog before it is spoken (as TS closes its session
    /// first), so a voice input barging in continues the dialog only if the
    /// answer asked for that
    async fn deliver(&self, part: AgentOutput) {
        let wrapped = part.directives.iter().filter_map(directives::to_wrapped_directive).collect();
        let mut station_directives = self.resolve(wrapped).await;
        if part.finished {
            if !part.text.is_empty() {
                station_directives.push(directives::tts_play_placeholder(None));
            }
        } else {
            let stage1 = directives::continue_session_stage1_frame(&self.target.dialog_id);
            station_directives.push(if part.text.is_empty() {
                directives::mm_semantic_frame(&stage1)
            } else {
                directives::tts_play_placeholder(Some(&stage1))
            });
        }

        let should_listen = part.finished && part.should_listen;
        if let Err(error) = self.outbox.alice_response(&self.target, &part.text, should_listen, station_directives) {
            tracing::warn!("answer not delivered: {error:#}");
            return;
        }
        if part.finished {
            self.dialog.finish_turn(&self.target.dialog_id, part.should_listen);
        }
        speak(&self.context, &self.outbox, &part.text, &self.target.reference_message_id).await;
    }

    /// Station directives, with the music actions turned into theirs (TS processWrappedDirectives)
    async fn resolve(&self, wrapped: Vec<WrappedDirective>) -> Vec<TDirective> {
        let offset_ms = self.device_state.as_deref().and_then(metadata::audio_offset_ms);
        let mut station_directives = Vec::new();
        for directive in wrapped {
            station_directives.extend(directive.station);
            if let Some(action) = directive.music {
                station_directives.extend(self.context.music.directives(&self.device_id, &action, offset_ms).await);
            }
        }
        station_directives
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

/// Calls the remote functions of one turn with its session id and metadata
struct RemoteExecutor {
    remote: Arc<RemoteServers>,
    session_id: String,
    metadata: Value,
}

impl FunctionExecutor for RemoteExecutor {
    /// Detached: neither the answer, nor an interrupted dialog, nor a closed
    /// connection waits for or cancels a call the LLM decided on; a scheduled
    /// call ("через час") sleeps in its own task
    fn spawn(&self, call: FunctionCall) {
        let (remote, session_id, metadata) = (self.remote.clone(), self.session_id.clone(), self.metadata.clone());
        tokio::spawn(async move {
            if let Some(delay_ms) = call.schedule_ms.filter(|milliseconds| *milliseconds > 0) {
                tracing::info!("calling {} after {delay_ms}ms", call.name);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            if let Err(error) = remote.call_by_name(&call.name, &call.arguments, &session_id, &metadata).await {
                tracing::warn!("remote function '{}' failed: {error:#}", call.name);
            }
        });
    }

    /// Part of the turn: an interrupted turn cancels it
    fn call(&self, call: FunctionCall) -> BoxFuture<'static, String> {
        let (remote, session_id, metadata) = (self.remote.clone(), self.session_id.clone(), self.metadata.clone());
        Box::pin(async move {
            match remote.call_by_name(&call.name, &call.arguments, &session_id, &metadata).await {
                Ok(result) => result,
                Err(error) => {
                    tracing::warn!("remote function '{}' failed: {error:#}", call.name);
                    format!("Error: {error:#}")
                }
            }
        })
    }
}
