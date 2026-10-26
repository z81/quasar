//! Everything the server sends on uni.ws goes through one writer task that
//! owns the WebSocket sink; tasks enqueue frames via a cloneable `Outbox`, so
//! no sender ever waits for the socket and frames of one task keep their order.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Result, anyhow};
use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::SinkExt;
use futures_util::stream::SplitSink;
use prost::Message as _;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::proto::n_alice::n_alice_api::t_alice_response_proto::t_voice_response::TOutputSpeech;
use crate::proto::n_alice::n_alice_api::t_alice_response_proto::{
    THeader as ResponseHeader, TResponse as ResponseBody, TVoiceResponse,
};
use crate::proto::n_alice::n_alice_api::t_server_message::{self, t_event::Event};
use crate::proto::n_alice::n_alice_api::t_stream_control::{self, t_action_close::EReason};
use crate::proto::n_alice::n_alice_api::{
    TAliceResponseProto, TAsrRecognition, TAsrResult, TDirective, TEventHeader, TLogAck, TPush, TServerMessage,
    TStreamControl, TTtsSpeak,
};
use crate::proto::n_matrix::n_api::TAnalyticsMetaInfo;

/// The first TTS stream gets 1025, as in TS
const FIRST_OUTPUT_STREAM_ID: i64 = 1024;

/// What an AliceResponse answers to
#[derive(Clone, Debug)]
pub struct ResponseTarget {
    pub dialog_id: String,
    pub request_id: String,
    pub sequence_number: u32,
    /// MessageId of the client event being answered (VoiceInput / TextInput)
    pub reference_message_id: String,
}

#[derive(Clone)]
pub struct Outbox {
    frames: mpsc::UnboundedSender<WsMessage>,
    last_stream_id: Arc<AtomicI64>,
}

/// Starts the writer task; it ends when the socket fails or every `Outbox` is dropped
pub fn spawn_writer(mut sink: SplitSink<WebSocket, WsMessage>) -> (Outbox, JoinHandle<()>) {
    let (frames, mut queue) = mpsc::unbounded_channel::<WsMessage>();
    let writer = tokio::spawn(async move {
        while let Some(frame) = queue.recv().await {
            if let Err(error) = sink.send(frame).await {
                tracing::warn!("uni.ws send failed: {error}");
                return;
            }
        }
        let _ = sink.close().await;
    });
    let outbox = Outbox { frames, last_stream_id: Arc::new(AtomicI64::new(FIRST_OUTPUT_STREAM_ID)) };
    (outbox, writer)
}

impl Outbox {
    fn send_binary(&self, frame: Vec<u8>) -> Result<()> {
        self.frames.send(WsMessage::Binary(frame.into())).map_err(|_| anyhow!("uni.ws connection is closed"))
    }

    pub fn send(&self, message: TServerMessage) -> Result<()> {
        let mut frame = Vec::with_capacity(message.encoded_len() + 4);
        frame.extend_from_slice(b"AAPI");
        message.encode(&mut frame)?;
        self.send_binary(frame)
    }

    fn send_event(&self, header: TEventHeader, event: Event) -> Result<()> {
        self.send(TServerMessage {
            message: Some(t_server_message::Message::Event(t_server_message::TEvent {
                header: Some(header),
                event: Some(event),
            })),
            ..Default::default()
        })
    }

    pub fn alice_response(
        &self,
        target: &ResponseTarget,
        text: &str,
        should_listen: bool,
        directives: Vec<TDirective>,
    ) -> Result<()> {
        self.voice_response(target, text, !text.is_empty(), should_listen, directives)
    }

    /// An AliceResponse that announces speech: `text` may be empty when the
    /// words are not known yet (they follow in the TTS stream)
    pub fn voice_response(
        &self,
        target: &ResponseTarget,
        text: &str,
        has_voice: bool,
        should_listen: bool,
        directives: Vec<TDirective>,
    ) -> Result<()> {
        self.send_event(
            reply_header(&target.reference_message_id),
            Event::AliceResponse(TAliceResponseProto {
                header: Some(ResponseHeader {
                    dialog_id: Some(target.dialog_id.clone()),
                    request_id: Some(target.request_id.clone()),
                    response_id: Some(Uuid::new_v4().to_string()),
                    sequence_number: Some(target.sequence_number),
                    ..Default::default()
                }),
                voice_response: Some(TVoiceResponse {
                    has_voice_response: has_voice,
                    should_listen,
                    output_speech: has_voice.then(|| TOutputSpeech { text: Some(text.to_string()) }),
                }),
                response: Some(ResponseBody { directives, is_streaming: false, ..Default::default() }),
                ..Default::default()
            }),
        )
    }

    /// A finished answer with nothing to say or do: the station stops waiting
    /// for this request instead of timing out
    pub fn empty_response(&self, target: &ResponseTarget) -> Result<()> {
        self.alice_response(target, "", false, Vec::new())
    }

    pub fn asr_result(&self, text: &str, reference_message_id: &str) -> Result<()> {
        self.send_event(
            reply_header(reference_message_id),
            Event::AsrResult(TAsrResult {
                end_of_utt: Some(true),
                recognition: if text.is_empty() {
                    Vec::new()
                } else {
                    vec![TAsrRecognition {
                        normalized: Some(text.to_string()),
                        confidence: Some(0.999),
                        parent_model: Some("openrouter-stt".to_string()),
                        ..Default::default()
                    }]
                },
                response_code: Some(0),
                messages_count: Some(0),
            }),
        )
    }

    /// The acknowledgement of a LogSpotter event (TS handleLogSpotterEvent)
    pub fn log_ack(&self, reference_message_id: &str) -> Result<()> {
        self.send_event(reply_header(reference_message_id), Event::LogAck(TLogAck {}))
    }

    /// Delivers directives outside of any request (TS sendPush)
    pub fn push(&self, directives: Vec<TDirective>) -> Result<()> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX));
        self.send_event(
            TEventHeader { message_id: Uuid::new_v4().to_string(), ack: Some(now_ms), ..Default::default() },
            Event::Push(TPush {
                analytics_meta_info: Some(TAnalyticsMetaInfo::default()),
                deduplication_push_id: Some(Uuid::new_v4().to_string()),
                directives,
                push_ids: vec![Uuid::new_v4().to_string()],
            }),
        )
    }

    /// Announces a LazyTtsStreaming OGG/Opus stream; it is closed when the
    /// returned guard is dropped, so an error or an aborted task closes it too
    pub fn open_tts_stream(&self, reference_message_id: &str) -> Result<TtsStream> {
        let stream_id = self.last_stream_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.send_event(
            TEventHeader { stream_id: Some(stream_id), ..reply_header(reference_message_id) },
            Event::TtsSpeak(TTtsSpeak {
                format: "audio/opus".to_string(),
                lazy_tts_streaming: true,
                ..Default::default()
            }),
        )?;
        Ok(TtsStream { outbox: self.clone(), stream_id })
    }
}

fn reply_header(reference_message_id: &str) -> TEventHeader {
    TEventHeader {
        message_id: Uuid::new_v4().to_string(),
        ref_message_id: reference_message_id.to_string(),
        ..Default::default()
    }
}

pub struct TtsStream {
    outbox: Outbox,
    stream_id: i64,
}

impl TtsStream {
    /// One audio frame: 4-byte big-endian stream id + OGG/Opus bytes
    pub fn send_audio(&self, chunk: &[u8]) -> Result<()> {
        let mut frame = Vec::with_capacity(4 + chunk.len());
        // stream ids are allocated from 1025 upwards and stay far below u32::MAX
        frame.extend_from_slice(&(self.stream_id as u32).to_be_bytes());
        frame.extend_from_slice(chunk);
        self.outbox.send_binary(frame)
    }
}

impl Drop for TtsStream {
    fn drop(&mut self) {
        let closed = self.outbox.send(TServerMessage {
            message: Some(t_server_message::Message::StreamControl(TStreamControl {
                stream_id: self.stream_id,
                message_id: Uuid::new_v4().to_string(),
                action: Some(t_stream_control::Action::Close(t_stream_control::TActionClose {
                    reason: EReason::Ok as i32,
                })),
            })),
            ..Default::default()
        });
        if closed.is_err() {
            tracing::debug!("TTS stream {} not closed: the connection is gone", self.stream_id);
        }
    }
}
