//! Dialogs and how requests are answered. A voice input or an external event
//! runs an agent turn whose answer parts (one per agent iteration) queue up;
//! every request of the station takes one part, as the TS InputHandler does:
//! the first part answers the request itself, later parts answer the
//! station's "continue" requests. A part that is not ready within 5 s is
//! replaced by a continue stage 1 server_action, so the station never waits
//! long for a response and comes back for the answer (stage 1 -> push of
//! stage 2 -> stage 2 request -> the part, or another 5 s wait). A part
//! whose text still streams from the LLM goes out with its header; its text
//! is spoken chunk by chunk as it arrives (see `speech`).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::metadata::{BiometrySlot, InputMetadata};
use super::outbox::{Outbox, ResponseTarget};
use super::speech::{self, SpeechEnd};
use super::{UniproxyContext, report};
use crate::agent::protocol::{DirectiveResult, FunctionCall};
use crate::agent::{Agent, AgentOutput, AgentTurn, ERROR_RESPONSE_TEXT, FunctionExecutor, OutputText};
use crate::audio;
use crate::connections::{ConnectionRegistry, StationCommand};
use crate::directives;
use crate::gigastt::{Endpoint, Recognition, Transcript};
use crate::mcp::{LiveContext, Mcp};
use crate::prompt::{State, system_state};
use crate::proto::n_alice::n_alice_api::TDirective;
use crate::remote::RemoteServers;
use crate::time_zone::Zone;

/// How long the end of the utterance waits for gigastt's final transcript
/// before OpenRouter transcribes the buffered audio instead
const FINAL_TRANSCRIPT_WAIT: Duration = Duration::from_secs(3);

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
    pub fn for_voice_input(
        &self,
        context: &UniproxyContext,
        station_dialog_id: Option<&str>,
        zone: &Zone,
    ) -> (String, Arc<Agent>) {
        let mut slot = self.lock();
        if let Some(dialog) = slot.as_mut() {
            let names_other = station_dialog_id.is_some_and(|id| !id.is_empty() && id != dialog.id);
            if dialog.awaiting_input && !names_other {
                dialog.awaiting_input = false;
                return (dialog.id.clone(), dialog.agent.clone());
            }
        }
        open(&mut slot, context, zone)
    }

    /// The dialog of an external event: like TS, the event joins the current
    /// agent session, under a new dialog id unless the request names the
    /// current one
    pub fn for_event(&self, context: &UniproxyContext, station_dialog_id: Option<&str>, zone: &Zone) -> (String, Arc<Agent>) {
        let mut slot = self.lock();
        match slot.as_mut() {
            Some(dialog) => {
                if station_dialog_id != Some(dialog.id.as_str()) {
                    dialog.id = Uuid::new_v4().to_string();
                }
                dialog.awaiting_input = false;
                (dialog.id.clone(), dialog.agent.clone())
            }
            None => open(&mut slot, context, zone),
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

fn open(slot: &mut Option<Dialog>, context: &UniproxyContext, zone: &Zone) -> (String, Arc<Agent>) {
    let dialog = Dialog {
        id: Uuid::new_v4().to_string(),
        agent: Arc::new(new_agent(context, zone)),
        awaiting_input: false,
        answers: None,
    };
    tracing::info!("dialog {} started", dialog.id);
    let opened = (dialog.id.clone(), dialog.agent.clone());
    *slot = Some(dialog);
    opened
}

fn new_agent(context: &UniproxyContext, zone: &Zone) -> Agent {
    let functions = context.functions_now();
    let system_prompt = context.prompt_generator.generate(&functions).unwrap_or_else(|error| {
        tracing::error!("failed to render the system prompt: {error:#}");
        String::new()
    });
    let state_prompt = context.prompt_generator.generate_state(&system_state(zone)).unwrap_or_else(|error| {
        tracing::error!("failed to render the state prompt: {error:#}");
        String::new()
    });
    Agent::new(
        context.openrouter.clone(),
        context.config.processor_model.clone(),
        functions,
        system_prompt,
        state_prompt,
        context.cache.clone(),
        context.config.tts_streaming,
    )
}

/// The audio of a finished VoiceInput and the speaker it came from
pub struct Utterance {
    pub ogg: Vec<u8>,
    pub biometry: BiometrySlot,
    /// gigastt's recognition of it, if that is the provider
    pub recognition: Option<Recognition>,
    /// the end of the utterance: gigastt's end of utterance or the station's Close
    pub ended_at: Instant,
    /// gigastt's end of utterance that ended it; its text already went to
    /// the station in the AsrResult
    pub endpoint: Option<Endpoint>,
}

/// A request of the station and what answering it needs
pub struct Request {
    pub context: Arc<UniproxyContext>,
    pub outbox: Outbox,
    pub dialog: DialogSlot,
    pub target: ResponseTarget,
    /// A voice request: when the user stopped speaking, for the latency of
    /// the answer's first audio
    pub speech_end: Option<SpeechEnd>,
    /// The station's time zone, for the time in the LLM's state
    pub zone: Zone,
    /// The MCP servers' live context for the turn's state, already on its way
    pub live_context: Option<LiveContext>,
    /// The station and its connections, for directives scheduled for later
    pub device_id: String,
    pub registry: ConnectionRegistry,
}

impl Request {
    /// VoiceInput: transcribe, report the recognition, answer through the agent
    pub async fn answer_voice(mut self, agent: Arc<Agent>, utterance: Utterance) {
        let Utterance { ogg, biometry, recognition, ended_at, endpoint } = utterance;
        let ended_by_gigastt = endpoint.is_some();
        let Transcript { text, speech_ended_at } = transcribe(&self.context, recognition, &ogg, ended_at, endpoint).await;
        // at gigastt's end of utterance the station got its AsrResult already
        if !ended_by_gigastt {
            if let Err(error) = self.outbox.asr_result(&text, &self.target.reference_message_id) {
                tracing::warn!("{error:#}");
                return;
            }
        }
        if text.is_empty() {
            // nothing recognized: close the request with an empty answer so the station does not wait for one
            report(self.outbox.empty_response(&self.target));
            self.dialog.finish_turn(&self.target.dialog_id, false);
            return;
        }
        // MatchedUser may arrive while the utterance is transcribed, so biometry is read only now (as in TS)
        let metadata = InputMetadata::new(biometry.get());
        self.speech_end = Some(SpeechEnd { speech: speech_ended_at, utterance: ended_at });
        let live_context = self.live_context.take();
        self.answer_with_agent(&agent, AgentTurn { external_event: false, text }, &metadata, live_context).await;
    }

    /// TextInput with an external event pushed through the API
    pub async fn answer_event(mut self, agent: Arc<Agent>, event: String) {
        let metadata = InputMetadata::new(None);
        let live_context = self.live_context.take();
        self.answer_with_agent(&agent, AgentTurn { external_event: true, text: event }, &metadata, live_context).await;
    }

    /// TextInput with text pushed through the API as if the user said it
    pub async fn answer_utterance(mut self, agent: Arc<Agent>, text: String) {
        let metadata = InputMetadata::new(None);
        let live_context = self.live_context.take();
        self.answer_with_agent(&agent, AgentTurn { external_event: false, text }, &metadata, live_context).await;
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
            Ok(()) => speech::speak_text(&self.context, &self.outbox, &text, &self.target.reference_message_id, None).await,
            Err(error) => tracing::warn!("{error:#}"),
        }
    }

    /// Text commands: directives without speech
    pub async fn answer_directives(self, directives: Vec<TDirective>) {
        report(self.outbox.alice_response(&self.target, "", false, directives));
    }

    /// Runs the agent turn while this request waits for its first part; the
    /// later parts wait for continue requests
    async fn answer_with_agent(
        &self,
        agent: &Arc<Agent>,
        turn: AgentTurn,
        metadata: &InputMetadata,
        live_context: Option<LiveContext>,
    ) {
        let (parts, answers) = mpsc::unbounded_channel();
        let answers: Answers = Arc::new(tokio::sync::Mutex::new(answers));
        self.dialog.set_answers(&self.target.dialog_id, answers.clone());
        tokio::join!(self.run_agent(agent, turn, metadata, live_context, parts), self.answer_next(&answers));
    }

    async fn run_agent(
        &self,
        agent: &Arc<Agent>,
        turn: AgentTurn,
        metadata: &InputMetadata,
        live_context: Option<LiveContext>,
        parts: mpsc::UnboundedSender<AgentOutput>,
    ) {
        let metadata_json = metadata.to_json();
        let session_id = agent.session_id().to_string();
        let state = dialog_state(&self.context, &session_id, metadata, &metadata_json, &self.zone, live_context).await;
        let state_prompt = self.context.prompt_generator.generate_state(&state).unwrap_or_else(|error| {
            tracing::error!("failed to render the state prompt: {error:#}");
            String::new()
        });
        let executor = RemoteExecutor {
            remote: self.context.remote.clone(),
            mcp: self.context.mcp.clone(),
            session_id,
            metadata: metadata_json,
            device_id: self.device_id.clone(),
            registry: self.registry.clone(),
        };
        if let Err(error) = agent.run_turn(turn, state_prompt, &executor, &parts).await {
            tracing::error!("agent turn failed: {error:#}");
            // like TS: an apology instead of silence, as the turn's last part
            let _ = parts.send(AgentOutput {
                directives: Vec::new(),
                finished: true,
                should_listen: false,
                text: OutputText::Complete(ERROR_RESPONSE_TEXT.to_string()),
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
    /// answer asked for that.
    ///
    /// A part whose text still streams goes out before its words are known:
    /// a voice response with an empty OutputSpeech text (the shape TS sends
    /// for an empty answer), the words follow in its TTS stream and the full
    /// text in the log; the station plays the stream via the placeholder
    async fn deliver(&self, part: AgentOutput) {
        let mut station_directives: Vec<TDirective> = part.directives.iter().filter_map(directives::to_directive).collect();
        let (shown_text, speaks) = match &part.text {
            OutputText::Complete(text) => (text.as_str(), !text.is_empty()),
            OutputText::Streaming(_) => ("", true),
        };
        if part.finished {
            if speaks {
                station_directives.push(directives::tts_play_placeholder(None));
            }
        } else {
            let stage1 = directives::continue_session_stage1_frame(&self.target.dialog_id);
            station_directives.push(if speaks {
                directives::tts_play_placeholder(Some(&stage1))
            } else {
                directives::mm_semantic_frame(&stage1)
            });
        }

        let should_listen = part.finished && part.should_listen;
        let sent = self.outbox.voice_response(&self.target, shown_text, speaks, should_listen, station_directives);
        if let Err(error) = sent {
            tracing::warn!("answer not delivered: {error:#}");
            return;
        }
        if part.finished {
            self.dialog.finish_turn(&self.target.dialog_id, part.should_listen);
        }
        let reference_message_id = &self.target.reference_message_id;
        match part.text {
            OutputText::Complete(text) => {
                speech::speak_text(&self.context, &self.outbox, &text, reference_message_id, self.speech_end).await
            }
            OutputText::Streaming(chunks) => {
                speech::speak(&self.context, &self.outbox, chunks, reference_message_id, self.speech_end).await
            }
        }
    }
}

/// The text of an utterance: gigastt's streamed recognition if it delivers in
/// time, otherwise the text of gigastt's end of utterance if there was one,
/// otherwise (and with STT_PROVIDER=openrouter) OpenRouter on the buffered audio
async fn transcribe(
    context: &UniproxyContext,
    recognition: Option<Recognition>,
    ogg: &[u8],
    ended_at: Instant,
    endpoint: Option<Endpoint>,
) -> Transcript {
    let buffered = |text| Transcript { text, speech_ended_at: None };
    let (transcript, provider) = match recognition {
        Some(recognition) => match recognition.finish(FINAL_TRANSCRIPT_WAIT.saturating_sub(ended_at.elapsed())).await {
            Ok(transcript) => (transcript, "gigastt"),
            Err(error) => match endpoint {
                Some(endpoint) => {
                    tracing::warn!("gigastt failed after its end of utterance ({error:#}), answering the text of that");
                    let transcript = Transcript { text: endpoint.text, speech_ended_at: endpoint.speech_ended_at };
                    (transcript, "gigastt (end of utterance)")
                }
                None => {
                    tracing::warn!("gigastt failed ({error:#}), transcribing with OpenRouter instead");
                    (buffered(transcribe_buffered(context, ogg).await), "OpenRouter (fallback)")
                }
            },
        },
        None => (buffered(transcribe_buffered(context, ogg).await), "OpenRouter"),
    };
    tracing::info!(
        "transcribed by {provider} {} ms after the end of the utterance: '{}'",
        ended_at.elapsed().as_millis(),
        transcript.text
    );
    transcript
}

/// Transcribes the whole buffered utterance with OpenRouter; failures count as "nothing heard"
async fn transcribe_buffered(context: &UniproxyContext, ogg: &[u8]) -> String {
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

/// The per-turn state shown to the LLM: system facts, the speaker, the MCP
/// servers' live context, remote state servers (system entries win over
/// remote duplicates, as in TS)
async fn dialog_state(
    context: &UniproxyContext,
    session_id: &str,
    metadata: &InputMetadata,
    metadata_json: &Value,
    zone: &Zone,
    live_context: Option<LiveContext>,
) -> State {
    let mut state = system_state(zone);
    let (age, gender) = metadata.biometry.map_or(("unknown", "unknown"), |biometry| (biometry.age, biometry.gender));
    state.insert("input_person_age".to_string(), ("age of person who talked to you".to_string(), age.to_string()));
    state.insert("input_person_gender".to_string(), ("gender of person who talked to you".to_string(), gender.to_string()));
    let live_context = async {
        match live_context {
            Some(live_context) => live_context.entries().await,
            None => Vec::new(),
        }
    };
    let (live, remote) = tokio::join!(live_context, context.remote.state(session_id, metadata_json));
    for (key, entry) in live.into_iter().chain(remote) {
        state.entry(key).or_insert(entry);
    }
    state
}

/// Calls the functions of one turn: the MCP servers' tools, the remote
/// function servers' functions with the turn's session id and metadata,
/// and the station directives scheduled for later
struct RemoteExecutor {
    remote: Arc<RemoteServers>,
    mcp: Arc<Mcp>,
    session_id: String,
    metadata: Value,
    device_id: String,
    registry: ConnectionRegistry,
}

impl RemoteExecutor {
    /// The call's result for the LLM; failures as text, never an error
    fn call_any(&self, call: FunctionCall) -> BoxFuture<'static, String> {
        let (remote, mcp, session_id, metadata) =
            (self.remote.clone(), self.mcp.clone(), self.session_id.clone(), self.metadata.clone());
        Box::pin(async move {
            // a function server's own function wins over an MCP prefix
            if !remote.owns(&call.name) && mcp.routes(&call.name) {
                return mcp.call(&call.name, &call.arguments).await;
            }
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

impl FunctionExecutor for RemoteExecutor {
    /// Detached: neither the answer, nor an interrupted dialog, nor a closed
    /// connection waits for or cancels a call the LLM decided on; a scheduled
    /// call ("через час") sleeps in its own task
    fn spawn(&self, call: FunctionCall) {
        let delay_ms = call.schedule_ms.filter(|milliseconds| *milliseconds > 0);
        let name = call.name.clone();
        let result = self.call_any(call);
        tokio::spawn(async move {
            if let Some(delay_ms) = delay_ms {
                tracing::info!("calling {name} after {delay_ms}ms");
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            result.await;
        });
    }

    /// Part of the turn: an interrupted turn cancels it
    fn call(&self, call: FunctionCall) -> BoxFuture<'static, String> {
        self.call_any(call)
    }

    /// Detached like `spawn`; it goes through the connection the station has then
    fn schedule_directive(&self, directive: DirectiveResult, delay_ms: u64) {
        let (device_id, registry) = (self.device_id.clone(), self.registry.clone());
        tracing::info!("sending {} after {delay_ms}ms", directive.name);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            if let Some(station_directive) = directives::to_directive(&directive) {
                let delivered = registry.send(&device_id, StationCommand::Directives { directives: vec![station_directive] });
                tracing::info!("pushed the scheduled {} to {delivered} connection(s) of {device_id}", directive.name);
            }
        });
    }
}
