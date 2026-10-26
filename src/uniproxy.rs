//! uni.ws: the station's WebSocket dialog channel. Binary frames are either
//! `AAPI` + protobuf TClientMessage or a 4-byte big-endian stream id + audio.
//!
//! A connection is a receive loop plus a writer task that owns the socket sink
//! (see `outbox`). The loop only dispatches: requests are answered in spawned
//! tasks (VoiceInput -> audio -> StreamControl close -> transcription -> agent
//! -> AliceResponse + streamed TTS), so the station's cancels, pings and new
//! requests are read while an answer is being prepared. A new VoiceInput or a
//! CANCEL aborts the dialog turns in flight, as TS interrupts its dialog.
//! Slow or multi-part answers go out through the TS "continue" handshake (see
//! `dialog`), music through the music manager.

mod dialog;
mod metadata;
mod outbox;
mod text_input;

use std::future::Future;
use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::StreamExt;
use prost::Message as _;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use uuid::Uuid;

use crate::agent::{Agent, Functions, ResponseCache};
use crate::config::Config;
use crate::connections::{ConnectionRegistry, StationCommand};
use crate::directives::{self, WrappedDirective};
use crate::music::MusicManager;
use crate::openrouter::OpenRouter;
use crate::prompt::PromptGenerator;
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
    pub functions: Functions,
    pub remote: Arc<crate::remote::RemoteServers>,
    pub cache: ResponseCache,
    pub music: MusicManager,
}

/// Guards memory against a voice stream that never ends: far more than any
/// spoken request in OGG/Opus; audio beyond it is dropped
const MAX_UTTERANCE_BYTES: usize = 8 * 1024 * 1024;

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
    let mut connection = Connection::new(context, outbox, &device_id);

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

/// A VoiceInput being recorded: the audio of its stream until StreamControl Close
struct VoiceInput {
    stream_id: i64,
    target: ResponseTarget,
    agent: Arc<Agent>,
    device_state: Option<Vec<u8>>,
    biometry: BiometrySlot,
    ogg: Vec<u8>,
    truncated: bool,
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
    }
}

struct Connection {
    context: Arc<UniproxyContext>,
    outbox: Outbox,
    /// The station (x-uprx-device-id): its music queue and saved position
    device_id: Arc<str>,
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
    /// To report audio of an unexpected stream once rather than per frame
    unknown_audio_stream: Option<i64>,
}

impl Connection {
    fn new(context: Arc<UniproxyContext>, outbox: Outbox, device_id: &str) -> Self {
        Self {
            context,
            outbox,
            device_id: device_id.into(),
            dialog: DialogSlot::default(),
            voice_input: None,
            biometry: BiometrySlot::default(),
            tasks: JoinSet::new(),
            turns: Vec::new(),
            answering_stream: None,
            unknown_audio_stream: None,
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
            StationCommand::Event { .. } | StationCommand::Speak { .. } => {
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
        let (dialog_id, agent) = self.dialog.for_voice_input(&self.context, request_header.dialog_id.as_deref());
        let stream_id = header.stream_id.unwrap_or(-1);
        tracing::info!("voice input stream {stream_id} started (dialog {dialog_id})");

        self.biometry = BiometrySlot::default();
        self.voice_input = Some(VoiceInput {
            stream_id,
            target: ResponseTarget {
                dialog_id,
                request_id: request_header.request_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
                sequence_number: request_header.sequence_number.unwrap_or(0),
                // AsrResult, AliceResponse and TTS refer to the VoiceInput event,
                // not to later events like MatchedUser or LogSpotter
                reference_message_id: header.message_id,
            },
            agent,
            device_state: metadata::take_device_state(&mut request),
            biometry: self.biometry.clone(),
            ogg: Vec::new(),
            truncated: false,
        });
    }

    fn handle_stream_control(&mut self, control: TStreamControl) {
        // only Close ends a voice input (other actions like SpotterEnd do not);
        // its reason OK is 0, so presence matters, not the value
        let Some(t_stream_control::Action::Close(close)) = control.action else {
            return;
        };
        let stream_id = control.stream_id;

        if close.reason == EReason::Cancel as i32 {
            let recording = self.voice_input.as_ref().is_some_and(|input| input.stream_id == stream_id);
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
            tracing::info!("voice input stream {stream_id} finished: {} bytes", input.ogg.len());
            let request = self.request(input.target, input.device_state);
            let utterance = Utterance { ogg: input.ogg, biometry: input.biometry };
            let task = self.spawn_turn(request.answer_voice(input.agent, utterance));
            self.answering_stream = Some((stream_id, task));
        }
    }

    fn handle_audio(&mut self, frame: &[u8]) {
        let Some((stream_id, audio)) = frame.split_first_chunk::<4>() else {
            return;
        };
        let stream_id = i64::from(u32::from_be_bytes(*stream_id));
        match self.voice_input.as_mut() {
            Some(input) if input.stream_id == stream_id => input.push_audio(audio),
            _ if self.unknown_audio_stream != Some(stream_id) => {
                self.unknown_audio_stream = Some(stream_id);
                tracing::debug!("dropping audio of stream {stream_id}: no voice input on it");
            }
            _ => {}
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
                let (dialog_id, agent) = self.dialog.for_event(&self.context, station_dialog_id);
                let request = self.request(target(dialog_id), device_state);
                self.spawn_turn(request.answer_event(agent, event));
            }
            TextInputKind::Tts(text) => {
                let request = self.request(target(self.dialog.response_dialog_id(station_dialog_id)), device_state);
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
                let request = self.request(target(self.dialog.response_dialog_id(dialog_id.as_deref())), device_state);
                self.spawn_turn(request.answer_continue());
            }
            TextInputKind::Text(text) => {
                let request = self.request(target(self.dialog.response_dialog_id(station_dialog_id)), device_state);
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
            TextInputKind::Music(action) => {
                tracing::info!("music request: {action:?}");
                let request = self.request(target(self.dialog.response_dialog_id(station_dialog_id)), device_state);
                let music = WrappedDirective { station: None, music: Some(action) };
                self.tasks.spawn(request.answer_directives(vec![music]));
            }
            TextInputKind::Unknown(description) => {
                // TS leaves such requests unanswered and the station waits 15 s for a timeout
                tracing::info!("unhandled TextInput ({description}) answered with nothing");
                report(self.outbox.empty_response(&target(self.dialog.response_dialog_id(station_dialog_id))));
            }
        }
    }

    fn request(&self, target: ResponseTarget, device_state: Option<Vec<u8>>) -> Request {
        Request {
            context: self.context.clone(),
            outbox: self.outbox.clone(),
            dialog: self.dialog.clone(),
            device_id: self.device_id.clone(),
            target,
            device_state,
        }
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

/// Sending fails only when the writer is gone, i.e. the connection is closing
fn report(sent: anyhow::Result<()>) {
    if let Err(error) = sent {
        tracing::warn!("{error:#}");
    }
}
