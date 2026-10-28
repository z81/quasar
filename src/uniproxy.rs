//! uni.ws: the station's WebSocket dialog channel. Binary frames are either
//! `AAPI` + protobuf TClientMessage or a 4-byte big-endian stream id + audio.
//!
//! A connection is a receive loop plus a writer task that owns the socket sink
//! (see `outbox`). The loop only dispatches: requests are answered in spawned
//! tasks (VoiceInput -> audio -> the end of the utterance -> transcription ->
//! agent -> AliceResponse + streamed TTS), so the station's cancels, pings and
//! new requests are read while an answer is being prepared. The utterance ends
//! with the station's StreamControl Close or, with gigastt and
//! STT_END_OF_SPEECH=server, as soon as gigastt hears the user stop speaking
//! (see `handle_endpoint`). A new VoiceInput or a CANCEL aborts the dialog
//! turns in flight, as TS interrupts its dialog.
//! Slow or multi-part answers go out through the TS "continue" handshake (see
//! `dialog`).

mod dialog;
mod metadata;
mod outbox;
mod speech;
mod text_input;

use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::StreamExt;
use prost::Message as _;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use uuid::Uuid;

use crate::agent::{Agent, Functions, ResponseCache};
use crate::config::Config;
use crate::connections::{ConnectionRegistry, StationCommand};
use crate::directives;
use crate::gigastt::{Endpoint, Gigastt, Recognition};
use crate::mcp::{LiveContext, Mcp};
use crate::openrouter::OpenRouter;
use crate::prompt::PromptGenerator;
use crate::time_zone::Zone;
use crate::proto::n_alice::TAliceRequestProto;
use crate::proto::n_alice::n_alice_api::t_client_message::{self, t_event::Event as ClientEvent};
use crate::proto::n_alice::n_alice_api::t_stream_control::{self, t_action_close::EReason};
use crate::proto::n_alice::n_alice_api::{TClientMessage, TEventHeader, TStreamControl};
use dialog::{DialogSlot, Request, Utterance};
use metadata::BiometrySlot;
use outbox::{Outbox, ResponseTarget};
use text_input::TextInputKind;

pub struct UniproxyContext {
    pub config: Config,
    pub openrouter: OpenRouter,
    pub prompt_generator: PromptGenerator,
    /// The station's functions and those of the remote function servers
    pub functions: Functions,
    pub remote: Arc<crate::remote::RemoteServers>,
    /// MCP servers: their tools as functions, their live context in the state
    pub mcp: Arc<Mcp>,
    pub cache: ResponseCache,
    /// streaming recognition (STT_PROVIDER=gigaam); None: OpenRouter after the end of speech
    pub gigastt: Option<Gigastt>,
    /// speech synthesis by Piper (TTS_PROVIDER=piper); None: OpenRouter
    pub piper: Option<crate::piper::Piper>,
}

impl UniproxyContext {
    /// The functions a new dialog gets: the station's, the function
    /// servers', and the tools the MCP servers list now
    pub fn functions_now(&self) -> Functions {
        let tools = self.mcp.functions();
        if tools.is_empty() {
            return self.functions.clone();
        }
        let mut functions = (*self.functions).clone();
        for (name, definition) in tools {
            functions.entry(name).or_insert(definition);
        }
        Arc::new(functions)
    }
}

/// Guards memory against a voice stream that never ends: far more than any
/// spoken request in OGG/Opus; audio beyond it is dropped
const MAX_UTTERANCE_BYTES: usize = 8 * 1024 * 1024;

/// What the station listens for; its voice stream starts with them
const WAKE_WORDS: [&str; 2] = ["алиса", "яндекс"];

pub async fn handle_socket(
    socket: WebSocket,
    context: Arc<UniproxyContext>,
    device_id: String,
    registry: ConnectionRegistry,
) {
    let (sink, mut receiver) = socket.split();
    let (outbox, mut writer) = outbox::spawn_writer(sink);
    let (command_sender, mut commands) = mpsc::unbounded_channel::<StationCommand>();
    let connection_id = registry.register(&device_id, command_sender);
    let mut connection = Connection::new(context, outbox);

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => connection.handle_command(command),
                None => break,
            },
            message = receiver.next() => match message {
                Some(Ok(WsMessage::Binary(payload))) => connection.handle_frame(&payload),
                Some(Ok(WsMessage::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    tracing::warn!("uni.ws receive error: {error}");
                    break;
                }
            },
            endpoint = next_endpoint(&mut connection.voice_input) => connection.handle_endpoint(endpoint),
            Some(finished) = connection.tasks.join_next(), if !connection.tasks.is_empty() => {
                if let Err(error) = finished {
                    if error.is_panic() {
                        tracing::error!("uni.ws task panicked: {error}");
                    }
                }
            }
            _ = &mut writer => {
                tracing::warn!("uni.ws writer stopped, closing the connection");
                break;
            }
        }
    }

    registry.unregister(&device_id, connection_id);
    // like TS handleClose: whatever the dialog was doing stops with the socket
    connection.tasks.abort_all();
}

/// The next end of utterance gigastt reports for the voice input being recorded
async fn next_endpoint(voice_input: &mut Option<VoiceInput>) -> Endpoint {
    if let Some(recognition) = voice_input.as_mut().and_then(|input| input.recognition.as_mut()) {
        if let Some(endpoint) = recognition.next_endpoint().await {
            return endpoint;
        }
    }
    std::future::pending().await
}

/// A VoiceInput being recorded: the audio of its stream until the end of the utterance
struct VoiceInput {
    stream_id: i64,
    started_at: Instant,
    target: ResponseTarget,
    /// the station's time zone when the request came
    zone: Zone,
    agent: Arc<Agent>,
    device_state: Option<Vec<u8>>,
    biometry: BiometrySlot,
    /// buffered for OpenRouter: the provider, or the fallback of gigastt
    ogg: Vec<u8>,
    truncated: bool,
    /// gigastt recognizing while the station records
    recognition: Option<Recognition>,
    /// the MCP servers' live context, fetched while the user speaks
    live_context: Option<LiveContext>,
}

/// The stream of an utterance gigastt ended while the station was recording.
/// TS forgets such a stream: its later audio is dropped, its Close or CANCEL
/// changes nothing
struct EndedStream {
    stream_id: i64,
    ended_at: Instant,
    /// audio that still came after the end
    late_audio_bytes: usize,
}

impl VoiceInput {
    fn push_audio(&mut self, audio: &[u8]) {
        if self.ogg.len() + audio.len() > MAX_UTTERANCE_BYTES {
            if !self.truncated {
                tracing::warn!("voice input stream {} is too long, dropping the rest", self.stream_id);
                self.truncated = true;
            }
            return;
        }
        self.ogg.extend_from_slice(audio);
        if let Some(recognition) = &self.recognition {
            recognition.push_audio(audio);
        }
    }
}

struct Connection {
    context: Arc<UniproxyContext>,
    outbox: Outbox,
    dialog: DialogSlot,
    voice_input: Option<VoiceInput>,
    /// Biometry of the latest voice input; MatchedUser may come after its Close
    biometry: BiometrySlot,
    /// Every task of the connection, aborted when it closes
    tasks: JoinSet<()>,
    /// Dialog turns in flight: a new VoiceInput or a CANCEL aborts them
    turns: Vec<AbortHandle>,
    /// The voice stream whose turn is in flight, for a CANCEL after its Close
    answering_stream: Option<(i64, AbortHandle)>,
    /// The latest stream that ended with gigastt's end of utterance
    ended_stream: Option<EndedStream>,
    /// To report audio of an unexpected stream once rather than per frame
    unknown_audio_stream: Option<i64>,
    /// The time zone the station named last (`Application.Timezone`)
    station_zone: Option<String>,
}

impl Connection {
    fn new(context: Arc<UniproxyContext>, outbox: Outbox) -> Self {
        Self {
            context,
            outbox,
            dialog: DialogSlot::default(),
            voice_input: None,
            biometry: BiometrySlot::default(),
            tasks: JoinSet::new(),
            turns: Vec::new(),
            answering_stream: None,
            ended_stream: None,
            unknown_audio_stream: None,
            station_zone: None,
        }
    }

    /// Management API commands: everything reaches the station as a Push
    fn handle_command(&mut self, command: StationCommand) {
        let pushed = match command {
            // TS pushEvent: the station sends the frame back as a TextInput, answered by the agent
            StationCommand::Event { text } if !text.trim().is_empty() => {
                vec![directives::mm_semantic_frame(&directives::external_event_frame(&text))]
            }
            // TS pushTts: the station sends the frame back as a TextInput, answered with the text spoken
            StationCommand::Speak { text } if !text.trim().is_empty() => {
                vec![directives::mm_semantic_frame(&directives::tts_frame(&text))]
            }
            // the station sends the frame back as a TextInput, answered like a recognized utterance
            StationCommand::Utterance { text } if !text.trim().is_empty() => {
                vec![directives::mm_semantic_frame(&directives::utterance_frame(&text))]
            }
            StationCommand::Event { .. } | StationCommand::Speak { .. } | StationCommand::Utterance { .. } => {
                tracing::warn!("not pushing an empty text to the station");
                return;
            }
            StationCommand::Directives { directives } => directives,
        };
        report(self.outbox.push(pushed));
    }

    fn handle_frame(&mut self, frame: &[u8]) {
        let Some(message) = frame.strip_prefix(b"AAPI") else {
            self.handle_audio(frame);
            return;
        };
        match TClientMessage::decode(message) {
            Ok(message) => self.handle_client_message(message),
            Err(error) => tracing::warn!("failed to decode TClientMessage: {error}"),
        }
    }

    fn handle_client_message(&mut self, message: TClientMessage) {
        match message.message {
            Some(t_client_message::Message::Event(event)) => {
                let header = event.header.unwrap_or_default();
                match event.event {
                    Some(ClientEvent::VoiceInput(request)) => self.handle_voice_input(header, request),
                    Some(ClientEvent::TextInput(request)) => self.handle_text_input(header, request),
                    Some(ClientEvent::LogSpotter(_)) => report(self.outbox.log_ack(&header.message_id)),
                    Some(ClientEvent::MatchedUser(request)) => self.handle_matched_user(&request),
                    _ => tracing::debug!("ignoring client event"),
                }
            }
            Some(t_client_message::Message::StreamControl(control)) => self.handle_stream_control(control),
            None => {}
        }
    }

    fn handle_voice_input(&mut self, header: TEventHeader, mut request: TAliceRequestProto) {
        if self.interrupt_turns() {
            tracing::info!("the dialog turn in flight was interrupted by a new voice input");
        }
        let request_header = request.header.take().unwrap_or_default();
        let zone = self.zone_for(&request);
        let (dialog_id, agent) = self.dialog.for_voice_input(&self.context, request_header.dialog_id.as_deref(), &zone);
        let stream_id = header.stream_id.unwrap_or(-1);
        tracing::info!("voice input stream {stream_id} started (dialog {dialog_id})");

        self.biometry = BiometrySlot::default();
        self.voice_input = Some(VoiceInput {
            stream_id,
            started_at: Instant::now(),
            target: ResponseTarget {
                dialog_id,
                request_id: request_header.request_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
                sequence_number: request_header.sequence_number.unwrap_or(0),
                // AsrResult, AliceResponse and TTS refer to the VoiceInput event,
                // not to later events like MatchedUser or LogSpotter
                reference_message_id: header.message_id,
            },
            zone,
            agent,
            device_state: metadata::take_device_state(&mut request),
            biometry: self.biometry.clone(),
            ogg: Vec::new(),
            truncated: false,
            recognition: self.context.gigastt.as_ref().map(Gigastt::start),
            live_context: self.context.mcp.live_context(),
        });
    }

    fn handle_stream_control(&mut self, control: TStreamControl) {
        // only Close ends a voice input (other actions like SpotterEnd do not);
        // its reason OK is 0, so presence matters, not the value
        let Some(t_stream_control::Action::Close(close)) = control.action else {
            return;
        };
        let stream_id = control.stream_id;
        let cancel = close.reason == EReason::Cancel as i32;
        let recording = self.voice_input.as_ref().is_some_and(|input| input.stream_id == stream_id);

        // after gigastt's end of utterance the stream is over for TS, which
        // ignores its Close and CANCEL: no second turn, the answer goes on
        if !recording {
            if let Some(ended) = self.ended_stream.take_if(|ended| ended.stream_id == stream_id) {
                let after = ended.ended_at.elapsed().as_millis();
                if cancel {
                    tracing::info!(
                        "voice input stream {stream_id} cancelled by the station {after} ms after its end of utterance: ignored"
                    );
                } else {
                    tracing::info!(
                        "voice input stream {stream_id} closed by the station {after} ms after its end of utterance \
                         ({} bytes of audio after it dropped)",
                        ended.late_audio_bytes
                    );
                }
                return;
            }
        }

        if cancel {
            let answering =
                self.answering_stream.as_ref().is_some_and(|(id, task)| *id == stream_id && !task.is_finished());
            if recording || answering {
                tracing::info!("voice input stream {stream_id} cancelled by the station");
                self.voice_input = None;
                self.interrupt_turns();
                self.dialog.close();
            }
            return;
        }

        if let Some(input) = self.voice_input.take_if(|input| input.stream_id == stream_id) {
            tracing::info!(
                "voice input stream {stream_id} finished by the station after {:.2} s: {} bytes",
                input.started_at.elapsed().as_secs_f64(),
                input.ogg.len()
            );
            let mut request = self.request(input.target, input.device_state, input.zone);
            request.live_context = input.live_context;
            let utterance = Utterance {
                ogg: input.ogg,
                biometry: input.biometry,
                recognition: input.recognition,
                ended_at: Instant::now(),
                endpoint: None,
            };
            let task = self.spawn_turn(request.answer_voice(input.agent, utterance));
            self.answering_stream = Some((stream_id, task));
        }
    }

    /// gigastt heard the user stop speaking. Unless nothing or only the wake
    /// word was said so far, that ends the utterance, as TS ends it at its
    /// STT's end of utterance: the station gets the AsrResult with the text at
    /// once (and stops recording), the turn starts without waiting for its Close
    fn handle_endpoint(&mut self, endpoint: Endpoint) {
        let Some(input) = self.voice_input.as_ref() else {
            return;
        };
        let stream_id = input.stream_id;
        let timing = format!("{}, stream open {:.2} s", endpoint.timing(), input.started_at.elapsed().as_secs_f64());
        if let Some(reason) = premature(&endpoint) {
            tracing::info!(
                "voice input stream {stream_id}: gigastt's end of utterance ({}) not taken, {reason} '{}' ({timing})",
                endpoint.reason,
                endpoint.text
            );
            return;
        }
        let Some(input) = self.voice_input.take() else {
            return;
        };
        tracing::info!(
            "voice input stream {stream_id}: end of utterance by gigastt ({}, {timing}): '{}'",
            endpoint.reason,
            endpoint.text
        );
        report(self.outbox.asr_result(&endpoint.text, &input.target.reference_message_id));
        self.ended_stream = Some(EndedStream { stream_id, ended_at: Instant::now(), late_audio_bytes: 0 });
        let mut request = self.request(input.target, input.device_state, input.zone);
        request.live_context = input.live_context;
        let utterance = Utterance {
            ogg: input.ogg,
            biometry: input.biometry,
            recognition: input.recognition,
            ended_at: Instant::now(),
            endpoint: Some(endpoint),
        };
        // not the answering stream: a later CANCEL of this stream is ignored
        self.spawn_turn(request.answer_voice(input.agent, utterance));
    }

    fn handle_audio(&mut self, frame: &[u8]) {
        let Some((stream_id, audio)) = frame.split_first_chunk::<4>() else {
            return;
        };
        let stream_id = i64::from(u32::from_be_bytes(*stream_id));
        match self.voice_input.as_mut() {
            Some(input) if input.stream_id == stream_id => input.push_audio(audio),
            _ => match self.ended_stream.as_mut() {
                // the station records on until it handles the end of utterance
                Some(ended) if ended.stream_id == stream_id => ended.late_audio_bytes += audio.len(),
                _ if self.unknown_audio_stream != Some(stream_id) => {
                    self.unknown_audio_stream = Some(stream_id);
                    tracing::debug!("dropping audio of stream {stream_id}: no voice input on it");
                }
                _ => {}
            },
        }
    }

    fn handle_matched_user(&self, request: &TAliceRequestProto) {
        if let Some(biometry) = metadata::biometry(request) {
            tracing::info!("speaker biometry: age {}, gender {}", biometry.age, biometry.gender);
            self.biometry.set(biometry);
        }
    }

    fn handle_text_input(&mut self, header: TEventHeader, mut request: TAliceRequestProto) {
        let request_header = request.header.take().unwrap_or_default();
        let station_dialog_id = request_header.dialog_id.as_deref();
        let zone = self.zone_for(&request);
        let kind = text_input::classify(&request);
        let device_state = metadata::take_device_state(&mut request);
        let target = |dialog_id: String| ResponseTarget {
            dialog_id,
            request_id: request_header.request_id.clone().unwrap_or_else(|| Uuid::new_v4().to_string()),
            sequence_number: request_header.sequence_number.unwrap_or(0),
            reference_message_id: header.message_id.clone(),
        };

        match kind {
            TextInputKind::ExternalEvent(event) => {
                tracing::info!("external event: '{event}'");
                let (dialog_id, agent) = self.dialog.for_event(&self.context, station_dialog_id, &zone);
                let mut request = self.request(target(dialog_id), device_state, zone);
                request.live_context = self.context.mcp.live_context();
                self.spawn_turn(request.answer_event(agent, event));
            }
            TextInputKind::Utterance(text) => {
                tracing::info!("typed utterance: '{text}'");
                let (dialog_id, agent) = self.dialog.for_voice_input(&self.context, station_dialog_id, &zone);
                let mut request = self.request(target(dialog_id), device_state, zone);
                request.live_context = self.context.mcp.live_context();
                self.spawn_turn(request.answer_utterance(agent, text));
            }
            TextInputKind::Tts(text) => {
                let request = self.request(target(self.dialog.response_dialog_id(station_dialog_id)), device_state, zone);
                self.tasks.spawn(request.answer_tts(text));
            }
            TextInputKind::ContinueStage1 { dialog_id } => {
                // as TS: the answer is a push of step 2 with the dialog id, the request gets no AliceResponse;
                // a step 1 of TS (without a dialog id) continues the current dialog
                let dialog_id = dialog_id.or_else(|| self.dialog.current_id());
                let frame = directives::continue_session_stage2_frame(dialog_id.as_deref());
                report(self.outbox.push(vec![directives::mm_semantic_frame(&frame)]));
            }
            TextInputKind::ContinueStage2 { dialog_id } => {
                // the next part of that dialog's answer; a part of an older dialog is never delivered
                let request = self.request(target(self.dialog.response_dialog_id(dialog_id.as_deref())), device_state, zone);
                self.spawn_turn(request.answer_continue());
            }
            TextInputKind::Text(text) => {
                let request = self.request(target(self.dialog.response_dialog_id(station_dialog_id)), device_state, zone);
                match directives::text_command(&text) {
                    Some(command) => {
                        self.tasks.spawn(request.answer_directives(vec![command]));
                    }
                    None => {
                        tracing::warn!("unknown text command '{text}'");
                        report(self.outbox.empty_response(&request.target));
                    }
                }
            }
            TextInputKind::Unknown(description) => {
                // TS leaves such requests unanswered and the station waits 15 s for a timeout
                tracing::info!("unhandled TextInput ({description}) answered with nothing");
                report(self.outbox.empty_response(&target(self.dialog.response_dialog_id(station_dialog_id))));
            }
        }
    }

    fn request(&self, target: ResponseTarget, device_state: Option<Vec<u8>>, zone: Zone) -> Request {
        Request {
            context: self.context.clone(),
            outbox: self.outbox.clone(),
            dialog: self.dialog.clone(),
            target,
            device_state,
            speech_end: None,
            zone,
            live_context: None,
        }
    }

    /// The time zone a request is answered in: the one it names, else the
    /// station's last known one (pushes and events may carry none)
    fn zone_for(&mut self, request: &TAliceRequestProto) -> Zone {
        let named = request.application.as_ref().and_then(|application| application.timezone.as_deref());
        if let Some(name) = named.filter(|name| self.station_zone.as_deref() != Some(*name)) {
            match self.station_zone.replace(name.to_string()) {
                None => tracing::info!("station time zone: '{name}'"),
                Some(previous) => tracing::info!("station time zone changed from '{previous}' to '{name}'"),
            }
        }
        Zone::of_station(self.station_zone.as_deref())
    }

    fn spawn_turn(&mut self, turn: impl Future<Output = ()> + Send + 'static) -> AbortHandle {
        self.turns.retain(|task| !task.is_finished());
        let task = self.tasks.spawn(turn);
        self.turns.push(task.clone());
        task
    }

    /// Aborts the dialog turns in flight (TS closes the processor session);
    /// returns whether one was still running
    fn interrupt_turns(&mut self) -> bool {
        self.answering_stream = None;
        let mut interrupted = false;
        for task in self.turns.drain(..) {
            if !task.is_finished() {
                task.abort();
                interrupted = true;
            }
        }
        interrupted
    }
}

/// Why gigastt's end of utterance is not the end of the request: it came
/// too soon after the latest word (the user may have spoken on after the
/// pause it was found at: gigastt takes it after all if silence follows, see
/// `gigastt::DeferredEnd`), the user has said nothing yet, or only the wake
/// word (people pause after «Алиса,» before the command)
fn premature(endpoint: &Endpoint) -> Option<&'static str> {
    let mut words =
        endpoint.text.split(|character: char| !character.is_alphanumeric()).filter(|word| !word.is_empty()).peekable();
    if endpoint.spoken_past() {
        Some("too little silence after its latest word yet")
    } else if words.peek().is_none() {
        Some("nothing recognized yet")
    } else if words.all(|word| WAKE_WORDS.contains(&word.to_lowercase().as_str())) {
        Some("only the wake word")
    } else {
        None
    }
}

/// Sending fails only when the writer is gone, i.e. the connection is closing
fn report(sent: anyhow::Result<()>) {
    if let Err(error) = sent {
        tracing::warn!("{error:#}");
    }
}
