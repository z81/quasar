//! The dialog agent: drives the LLM in the JSON protocol, executes function
//! calls (station directives pass through, remote functions may answer and
//! feed results back into the conversation), caches idempotent first-turn
//! answers. An answer that writes its `text` last is followed while it
//! streams: its part goes out as soon as the members before the text are
//! known, and the text follows in speakable chunks.

pub mod protocol;
mod stream;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::future::BoxFuture;
use lru::LruCache;
use protocol::{DirectiveResult, FunctionCall, StructuredResponse};
use serde_json::Value;
use stream::{AnswerScanner, TextChunker};
use tokio::sync::{Mutex, mpsc};

use crate::openrouter::{ChatMessage, ChatStream, OpenRouter};

const MAX_AGENT_ITERATIONS: usize = 5;
const ITERATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Spoken when the LLM fails, instead of leaving the station silent (as in TS)
pub const ERROR_RESPONSE_TEXT: &str = "Извините, что-то пошло не так. Попробуйте спросить ещё раз.";

/// A function the LLM can call: either a station directive (pass-through) or
/// a remote callable
#[derive(Clone)]
pub enum FunctionKind {
    Directive,
    Remote { has_response: bool },
}

#[derive(Clone)]
pub struct FunctionDefinition {
    pub description: String,
    pub kind: FunctionKind,
    pub arguments_json_schema: Option<Value>,
}

pub type Functions = Arc<std::collections::HashMap<String, FunctionDefinition>>;

/// Answers to first requests of a dialog, shared by all dialogs (as in TS);
/// None: PROCESSOR_CACHE_SIZE=0, nothing is looked up or stored
pub type ResponseCache = Option<Arc<Mutex<LruCache<String, String>>>>;

pub fn new_response_cache(size: usize) -> ResponseCache {
    match std::num::NonZeroUsize::new(size) {
        Some(size) => {
            tracing::info!("response cache: the answers to up to {size} first requests (PROCESSOR_CACHE_SIZE)");
            Some(Arc::new(Mutex::new(LruCache::new(size))))
        }
        None => {
            tracing::info!("response cache off (PROCESSOR_CACHE_SIZE=0): every request goes to the LLM");
            None
        }
    }
}

/// One spoken answer part with the directives collected along the way
pub struct AgentOutput {
    pub directives: Vec<DirectiveResult>,
    /// true when no function results are pending (the turn is complete)
    pub finished: bool,
    pub should_listen: bool,
    pub text: OutputText,
}

/// The text of an answer part
pub enum OutputText {
    /// Known whole once the LLM's answer was complete
    Complete(String),
    /// Speakable chunks in order while the LLM still writes; the channel
    /// closes once the text is complete
    Streaming(mpsc::UnboundedReceiver<String>),
}

/// Pending results of has-response functions, by function name
type FunctionResults = Vec<(String, BoxFuture<'static, String>)>;

/// Runs the remote functions the LLM calls
pub trait FunctionExecutor: Send + Sync {
    /// A call whose result the LLM does not need: it runs in the background
    /// (after `schedule_ms`, if set) and nothing waits for it
    fn spawn(&self, call: FunctionCall);

    /// A call whose result goes back to the LLM
    fn call(&self, call: FunctionCall) -> BoxFuture<'static, String>;
}

pub struct Agent {
    openrouter: OpenRouter,
    model: String,
    functions: Functions,
    cache: ResponseCache,
    state: Arc<Mutex<AgentState>>,
    /// held for a whole turn: turns of one dialog run one after another, like
    /// the turn queue of the TS session
    turn: Mutex<()>,
    /// identifies the dialog to remote function and state servers (TS: a UUID per session)
    session_id: String,
    /// follow the answers' text while it streams (OPENROUTER_TTS_STREAMING)
    stream_text: bool,
}

struct AgentState {
    messages: Vec<ChatMessage>,
    first_request: bool,
}

pub struct AgentTurn {
    pub external_event: bool,
    pub text: String,
}

impl Agent {
    pub fn new(
        openrouter: OpenRouter,
        model: String,
        functions: Functions,
        system_prompt: String,
        initial_state_prompt: String,
        cache: ResponseCache,
        stream_text: bool,
    ) -> Self {
        let state = Arc::new(Mutex::new(AgentState {
            messages: vec![
                ChatMessage::system(system_prompt),
                ChatMessage::system(initial_state_prompt),
            ],
            first_request: true,
        }));
        Self {
            openrouter,
            model,
            functions,
            cache,
            state,
            turn: Mutex::new(()),
            session_id: uuid::Uuid::new_v4().to_string(),
            stream_text,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Runs one dialog turn; every agent iteration sends its answer part to
    /// `outputs` as soon as it is known, before the function results it waits
    /// for (TS emits its partial responses the same way). The last part is
    /// `finished`; on an error no further part follows
    pub async fn run_turn(
        self: &Arc<Self>,
        turn: AgentTurn,
        state_prompt: String,
        executor: &dyn FunctionExecutor,
        outputs: &mpsc::UnboundedSender<AgentOutput>,
    ) -> Result<()> {
        let _turn = self.turn.lock().await;
        let text = turn.text.trim().to_string();

        let first_request = {
            let mut state = self.state.lock().await;
            state.messages.push(ChatMessage::system(state_prompt));
            state.messages.push(if turn.external_event {
                ChatMessage::system(format!("External event happened: '{text}'"))
            } else {
                ChatMessage::user(text.clone())
            });
            state.first_request
        };

        // only the first request of a dialog is context-free enough to reuse
        let cached = match &self.cache {
            Some(cache) if first_request => cache.lock().await.get(&text).cloned(),
            _ => None,
        };

        for iteration in 0..MAX_AGENT_ITERATIONS {
            let mut answer = StreamedAnswer::default();
            let received = match cached.as_deref().filter(|_| iteration == 0) {
                // a cached answer takes the same path as a streamed one
                Some(cached) => {
                    self.follow(&mut answer, cached, executor, outputs);
                    Ok(cached.to_string())
                }
                None => self.receive(&mut answer, executor, outputs).await,
            };
            let response = match received {
                Ok(response) => response,
                Err(error) => return failed_after_header(answer.part, error),
            };
            tracing::info!("LLM answer: {response}");

            let structured = match protocol::parse_llm_response(&response) {
                Ok(structured) => structured,
                Err(error) => {
                    if answer.part.is_some() {
                        self.state.lock().await.messages.push(ChatMessage::assistant(response));
                    }
                    return failed_after_header(answer.part, error);
                }
            };

            let (finished, results, complete_part) = match answer.part.take() {
                // the part went out with its header
                Some(part) => (part.finished, part.results, None),
                None => {
                    let (directives, results) = self.run_calls(&structured.function_calls, executor);
                    let finished = results.is_empty();
                    let part = AgentOutput {
                        directives,
                        finished,
                        should_listen: structured.require_more_input,
                        text: OutputText::Complete(structured.text.clone()),
                    };
                    (finished, results, Some(part))
                }
            };
            self.remember(&text, response, &structured, finished, cached.is_none()).await;
            tracing::info!("agent {}: '{}'", if finished { "finished" } else { "answered a part" }, structured.text);
            if let Some(part) = complete_part {
                // nobody is left to take the answer (the dialog was dropped): stop
                if outputs.send(part).is_err() {
                    return Ok(());
                }
            }
            if finished {
                return Ok(());
            }

            let results = futures_util::future::join_all(
                results.into_iter().map(|(name, result)| async move { format!("{name}: {}", result.await) }),
            )
            .await;
            self.state.lock().await.messages.push(ChatMessage::system(results.join("\n")));
        }

        tracing::warn!("agent reached the iteration limit");
        let _ = outputs.send(AgentOutput {
            directives: Vec::new(),
            finished: true,
            should_listen: false,
            text: OutputText::Complete(String::new()),
        });
        Ok(())
    }

    /// Streams the LLM's answer, following every piece as it arrives
    async fn receive(
        &self,
        answer: &mut StreamedAnswer,
        executor: &dyn FunctionExecutor,
        outputs: &mpsc::UnboundedSender<AgentOutput>,
    ) -> Result<String> {
        let messages = self.state.lock().await.messages.clone();
        let mut stream = tokio::time::timeout(
            ITERATION_TIMEOUT,
            self.openrouter.chat_stream(&self.model, &messages),
        )
        .await??;

        let mut response = String::new();
        let read = async {
            use futures_util::StreamExt;
            while let Some(delta) = stream.next().await {
                let delta = delta?.0;
                self.follow(answer, &delta, executor, outputs);
                response.push_str(&delta);
            }
            Ok::<(), anyhow::Error>(())
        };
        let read = tokio::time::timeout(ITERATION_TIMEOUT, read).await;
        self.log_timing(&stream, answer.text_started_at);
        read??;
        Ok(response)
    }

    /// How long an answer took, in ms since its request: OpenRouter's
    /// response, the first SSE event of any kind (a reasoning model may
    /// stream its reasoning first), the first content, the start of the
    /// spoken text, the end
    fn log_timing(&self, stream: &ChatStream, text_started_at: Option<Instant>) {
        let timing = stream.timing();
        let ms = |duration: Option<Duration>| duration.map_or_else(|| "-".to_string(), |duration| duration.as_millis().to_string());
        tracing::info!(
            "LLM {} (reasoning {}): response {} ms, first delta {} ms, first content {} ms, text {} ms, done {} ms",
            self.model,
            stream.reasoning().unwrap_or("default"),
            timing.response.as_millis(),
            ms(timing.first_event),
            ms(timing.first_content),
            ms(text_started_at.map(|at| at.saturating_duration_since(timing.started))),
            timing.started.elapsed().as_millis()
        );
    }

    /// Follows one piece of the answer: once `function_calls`,
    /// `continue_dialog` and `can_cache` are complete and the text began, the
    /// part goes out and its text follows chunk by chunk. A text written
    /// before them cannot be streamed: the answer is then taken whole
    fn follow(
        &self,
        answer: &mut StreamedAnswer,
        piece: &str,
        executor: &dyn FunctionExecutor,
        outputs: &mpsc::UnboundedSender<AgentOutput>,
    ) {
        let scanned = answer.scanner.push(piece);
        if scanned.text_started {
            answer.text_started_at = Some(Instant::now());
        }
        if scanned.text_started && self.stream_text {
            match header(&answer.scanner.fields) {
                Some((calls, continue_dialog)) => {
                    let (directives, results) = self.run_calls(&calls, executor);
                    let finished = results.is_empty();
                    let (chunks, receiver) = mpsc::unbounded_channel();
                    let _ = outputs.send(AgentOutput {
                        directives,
                        finished,
                        should_listen: continue_dialog,
                        text: OutputText::Streaming(receiver),
                    });
                    answer.part = Some(StreamingPart {
                        finished,
                        results,
                        chunker: TextChunker::default(),
                        chunks: Some(chunks),
                        spoke: false,
                    });
                }
                None => tracing::info!("the answer writes its text first: it is spoken once complete"),
            }
        }
        if let Some(part) = answer.part.as_mut() {
            part.push(&scanned.text);
            if scanned.text_ended {
                part.end_text();
            }
        }
    }

    /// Station directives of the calls; remote calls start now: those
    /// without a result in the background, those with one as futures the
    /// turn waits for
    fn run_calls(
        &self,
        calls: &[FunctionCall],
        executor: &dyn FunctionExecutor,
    ) -> (Vec<DirectiveResult>, FunctionResults) {
        let (mut directives, mut results) = (Vec::new(), Vec::new());
        for call in calls {
            match self.functions.get(&call.name) {
                None => tracing::warn!("LLM tried to call unknown function '{}'", call.name),
                Some(definition) => match &definition.kind {
                    FunctionKind::Directive => directives.push(DirectiveResult {
                        name: call.name.clone(),
                        arguments: call.arguments.clone(),
                    }),
                    FunctionKind::Remote { has_response: true } if call.schedule_ms.is_some() => {
                        tracing::warn!("LLM tried to schedule function with response: '{}'", call.name);
                    }
                    FunctionKind::Remote { has_response: true } => {
                        results.push((call.name.clone(), executor.call(call.clone())));
                    }
                    FunctionKind::Remote { has_response: false } => executor.spawn(call.clone()),
                },
            }
        }
        (directives, results)
    }

    /// The answer joins the history; a complete first answer that may be
    /// reused goes to the cache
    async fn remember(
        &self,
        request: &str,
        response: String,
        structured: &StructuredResponse,
        finished: bool,
        fresh: bool,
    ) {
        let mut state = self.state.lock().await;
        if let Some(cache) = &self.cache {
            if !structured.require_more_input && structured.can_cache && state.first_request && finished && fresh {
                cache.lock().await.put(request.to_string(), response.clone());
            }
        }
        state.messages.push(ChatMessage::assistant(response));
        state.first_request = false;
    }
}

/// An answer that failed: before its part went out the error stands (the
/// dialog apologizes); after it, the turn ends with what was said, a text
/// broken off mid-way losing its unfinished end. The apology is spoken in
/// the part's stream only if nothing was said, never on top
fn failed_after_header(part: Option<StreamingPart>, error: anyhow::Error) -> Result<()> {
    let Some(mut part) = part else {
        return Err(error);
    };
    tracing::warn!("the LLM answer failed after its part went out: {error:#}");
    if !part.spoke {
        part.send(ERROR_RESPONSE_TEXT.to_string());
    }
    // dropping the part closes its text
    Ok(())
}

/// The part's header once the members before the text are known
fn header(fields: &serde_json::Map<String, Value>) -> Option<(Vec<FunctionCall>, bool)> {
    let calls = protocol::parse_function_calls(fields.get("function_calls")?).ok()?;
    fields.get("can_cache")?.as_bool()?;
    Some((calls, fields.get("continue_dialog")?.as_bool()?))
}

/// One LLM answer while it arrives
#[derive(Default)]
struct StreamedAnswer {
    scanner: AnswerScanner,
    /// The part that went out as soon as its header was known
    part: Option<StreamingPart>,
    /// When the `text` string began
    text_started_at: Option<Instant>,
}

/// A part whose header is out while its text still streams
struct StreamingPart {
    finished: bool,
    results: FunctionResults,
    chunker: TextChunker,
    /// Dropped once the text is complete (or the answer failed), which ends
    /// the part's speech
    chunks: Option<mpsc::UnboundedSender<String>>,
    /// A chunk went to the speech
    spoke: bool,
}

impl StreamingPart {
    fn push(&mut self, text: &str) {
        if let Some(chunk) = self.chunker.push(text) {
            self.send(chunk);
        }
    }

    /// The text is complete: its speech can end before the rest of the
    /// answer arrives. An empty text keeps the speech open until the answer
    /// is known to be valid, so a broken one can still be apologized for
    fn end_text(&mut self) {
        if let Some(rest) = self.chunker.finish() {
            self.send(rest);
        }
        if self.spoke {
            self.chunks = None;
        }
    }

    fn send(&mut self, chunk: String) {
        // a speech that is gone (its request was dropped) does not stop the answer
        if let Some(chunks) = &self.chunks {
            self.spoke |= chunks.send(chunk).is_ok();
        }
    }
}
