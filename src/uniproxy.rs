//! uni.ws: the station's WebSocket dialog channel. Binary frames are either
//! `AAPI` + protobuf TClientMessage or a 4-byte big-endian stream id + audio.
//! The voice pipeline: VoiceInput event -> audio chunks -> StreamControl
//! close -> OpenRouter transcription -> agent -> AliceResponse + streamed TTS.

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use prost::Message as _;
use uuid::Uuid;

use crate::agent::{Agent, AgentTurn, Functions};
use crate::audio;
use crate::config::Config;
use crate::openrouter::OpenRouter;
use crate::proto::n_alice::n_alice_api::t_client_message;
use crate::proto::n_alice::n_alice_api::t_server_message;
use crate::proto::n_alice::n_alice_api::t_alice_response_proto::t_voice_response::TOutputSpeech;
use crate::proto::n_alice::n_alice_api::t_alice_response_proto::{
    THeader as ResponseHeader, TResponse as ResponseBody, TVoiceResponse,
};
use crate::proto::n_alice::n_alice_api::{
    TAsrRecognition, TAsrResult, TAliceResponseProto, TClientMessage, TDirective, TEventHeader,
    TServerMessage, TTtsSpeak,
};
use crate::prompt::{system_state, PromptGenerator};

pub struct UniproxyContext {
    pub config: Config,
    pub openrouter: OpenRouter,
    pub prompt_generator: PromptGenerator,
    pub functions: Functions,
}

struct VoiceInputSession {
    audio_chunks: Vec<Vec<u8>>,
    dialog_id: String,
    request_id: String,
    sequence_number: u32,
    stream_id: i64,
}

pub async fn handle_socket(socket: WebSocket, context: Arc<UniproxyContext>) {
    let (mut sender, mut receiver) = socket.split();
    let mut voice_input: Option<VoiceInputSession> = None;
    let mut reference_message_id = String::new();
    let mut output_stream_id: i64 = 1024;
    let mut agent: Option<Arc<Agent>> = None;

    while let Some(message) = receiver.next().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!("uni.ws receive error: {error}");
                break;
            }
        };
        let payload = match message {
            WsMessage::Binary(payload) => payload,
            WsMessage::Close(_) => break,
            _ => continue,
        };

        if payload.starts_with(b"AAPI") {
            let client_message = match TClientMessage::decode(&payload[4..]) {
                Ok(client_message) => client_message,
                Err(error) => {
                    tracing::warn!("failed to decode TClientMessage: {error}");
                    continue;
                }
            };
            match client_message.message {
                Some(t_client_message::Message::Event(event)) => {
                    if let Some(header) = &event.header {
                        reference_message_id = header.message_id.clone();
                    }
                    match event.event {
                        Some(t_client_message::t_event::Event::VoiceInput(request)) => {
                            let request_header =
                                request.header.unwrap_or_default();
                            let stream_id = event
                                .header
                                .as_ref()
                                .and_then(|header| header.stream_id)
                                .unwrap_or(-1);
                            tracing::info!("voice input stream {stream_id} started");
                            voice_input = Some(VoiceInputSession {
                                audio_chunks: Vec::new(),
                                dialog_id: request_header
                                    .dialog_id
                                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
                                request_id: request_header
                                    .request_id
                                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
                                sequence_number: request_header.sequence_number.unwrap_or(0),
                                stream_id,
                            });
                        }
                        _ => {
                            // other client events (LogSpotter, TextInput, ...) arrive
                            // only once the HTTP-side emulation is ported
                            tracing::debug!("ignoring client event");
                        }
                    }
                }
                Some(t_client_message::Message::StreamControl(stream_control)) => {
                    let closed_voice_stream = voice_input
                        .as_ref()
                        .map(|session| session.stream_id == stream_control.stream_id)
                        .unwrap_or(false);
                    if closed_voice_stream {
                        let session = voice_input.take().expect("checked above");
                        let current_agent = agent.get_or_insert_with(|| {
                            Arc::new(Agent::new(
                                context.openrouter.clone(),
                                context.config.processor_model.clone(),
                                context.functions.clone(),
                                context.prompt_generator.generate(&context.functions).unwrap_or_default(),
                                context
                                    .prompt_generator
                                    .generate_state(&system_state())
                                    .unwrap_or_default(),
                                context.config.processor_cache_size as usize,
                            ))
                        })
                        .clone();

                        let outcome = finish_voice_input(
                            &context, &mut sender, &session, &reference_message_id,
                            &mut output_stream_id, &current_agent,
                        )
                        .await;
                        if let Err(error) = outcome {
                            tracing::error!("voice input processing failed: {error:#}");
                        }
                    }
                }
                None => {}
            }
            continue;
        }

        // audio frame: 4-byte big-endian stream id + OGG/Opus bytes
        if payload.len() < 4 {
            continue;
        }
        let stream_id = i64::from(u32::from_be_bytes(
            payload[0..4].try_into().expect("four bytes"),
        ));
        if let Some(session) = voice_input.as_mut() {
            if session.stream_id == stream_id {
                session.audio_chunks.push(payload[4..].to_vec());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn finish_voice_input(
    context: &Arc<UniproxyContext>,
    sender: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    session: &VoiceInputSession,
    reference_message_id: &str,
    output_stream_id: &mut i64,
    agent: &Arc<Agent>,
) -> Result<()> {
    // 1. transcribe the whole utterance
    let ogg: Vec<u8> = session.audio_chunks.iter().flatten().copied().collect();
    let mut text = String::new();
    if !ogg.is_empty() {
        let wav = audio::ogg_to_wav(&ogg).await.context("transcoding utterance")?;
        text = context
            .openrouter
            .transcribe(&context.config.stt_model, &context.config.stt_language, &wav)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!("transcription failed: {error:#}");
                String::new()
            });
    }
    tracing::info!("transcribed: '{text}'");

    send_asr_result(sender, &text, reference_message_id).await?;

    if text.is_empty() {
        return Ok(());
    }

    // 2. run the agent
    let state = {
        let mut state = system_state();
        state.insert("input_person_gender".to_string(), "unknown".to_string());
        state
    };
    let state_prompt = context.prompt_generator.generate_state(&state)?;
    let outputs = agent
        .run_turn(
            AgentTurn { external_event: false, text },
            state_prompt,
            |_call| Ok(()), // remote function servers are not ported yet
        )
        .await
        .context("agent turn failed")?;

    // 3. answer: AliceResponse + streamed TTS audio per output
    for output in outputs {
        let directives: Vec<TDirective> = output
            .directives
            .iter()
            .map(|directive| TDirective {
                name: directive.name.clone(),
                r#type: "client_action".to_string(),
                ..Default::default()
            })
            .collect();

        send_server_message(
            sender,
            TServerMessage {
                message: Some(t_server_message::Message::Event(t_server_message::TEvent {
                    header: Some(TEventHeader {
                        message_id: Uuid::new_v4().to_string(),
                        ref_message_id: reference_message_id.to_string(),
                        ..Default::default()
                    }),
                    event: Some(t_server_message::t_event::Event::AliceResponse(
                        TAliceResponseProto {
                            header: Some(ResponseHeader {
                                dialog_id: Some(session.dialog_id.clone()),
                                request_id: Some(session.request_id.clone()),
                                response_id: Some(Uuid::new_v4().to_string()),
                                sequence_number: Some(session.sequence_number),
                                ..Default::default()
                            }),
                            voice_response: Some(TVoiceResponse {
                                has_voice_response: !output.text.is_empty(),
                                should_listen: output.should_listen,
                                output_speech: (!output.text.is_empty()).then_some(TOutputSpeech {
                                    text: Some(output.text.clone()),
                                }),
                                ..Default::default()
                            }),
                            response: Some(ResponseBody {
                                directives,
                                is_streaming: false,
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                })),
                ..Default::default()
            },
        )
        .await?;

        if output.text.is_empty() {
            continue;
        }

        // stream synthesized speech chunk by chunk
        *output_stream_id += 1;
        let stream_id = *output_stream_id;
        send_server_message(
            sender,
            TServerMessage {
                message: Some(t_server_message::Message::Event(t_server_message::TEvent {
                    header: Some(TEventHeader {
                        message_id: Uuid::new_v4().to_string(),
                        ref_message_id: reference_message_id.to_string(),
                        stream_id: Some(stream_id),
                        ..Default::default()
                    }),
                    event: Some(t_server_message::t_event::Event::TtsSpeak(TTtsSpeak {
                        format: "audio/opus".to_string(),
                        lazy_tts_streaming: true,
                        ..Default::default()
                    })),
                })),
                ..Default::default()
            },
        )
        .await?;

        let pcm = context
            .openrouter
            .speech_stream(
                &context.config.tts_model,
                &context.config.tts_voice,
                &context.config.tts_instructions,
                1.0,
                &output.text,
            )
            .await
            .context("speech synthesis request failed")?;
        let ogg_stream = audio::pcm_stream_to_ogg(pcm, context.config.tts_pcm_sample_rate);
        tokio::pin!(ogg_stream);
        while let Some(chunk) = ogg_stream.next().await {
            let chunk = chunk.context("tts transcode failed")?;
            let mut audio_frame = Vec::with_capacity(4 + chunk.len());
            audio_frame.extend_from_slice(&(stream_id as u32).to_be_bytes());
            audio_frame.extend_from_slice(&chunk);
            sender.send(WsMessage::Binary(audio_frame.into())).await?;
        }

        send_server_message(
            sender,
            TServerMessage {
                message: Some(t_server_message::Message::StreamControl(
                    crate::proto::n_alice::n_alice_api::TStreamControl {
                        stream_id,
                        message_id: Uuid::new_v4().to_string(),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            },
        )
        .await?;
    }

    Ok(())
}

async fn send_asr_result(
    sender: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    text: &str,
    reference_message_id: &str,
) -> Result<()> {
    send_server_message(
        sender,
        TServerMessage {
            message: Some(t_server_message::Message::Event(t_server_message::TEvent {
                header: Some(TEventHeader {
                    message_id: Uuid::new_v4().to_string(),
                    ref_message_id: reference_message_id.to_string(),
                    ..Default::default()
                }),
                event: Some(t_server_message::t_event::Event::AsrResult(TAsrResult {
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
                })),
            })),
            ..Default::default()
        },
    )
    .await
}

async fn send_server_message(
    sender: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    server_message: TServerMessage,
) -> Result<()> {
    let mut frame = Vec::with_capacity(server_message.encoded_len() + 4);
    frame.extend_from_slice(b"AAPI");
    server_message.encode(&mut frame)?;
    sender.send(WsMessage::Binary(frame.into())).await?;
    Ok(())
}
