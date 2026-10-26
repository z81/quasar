//! The dialog agent: drives the LLM in the JSON protocol, executes function
//! calls (station directives pass through, remote functions may answer and
//! feed results back into the conversation), caches idempotent first-turn
//! answers.

pub mod protocol;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures_util::future::BoxFuture;
use lru::LruCache;
use protocol::{DirectiveResult, FunctionCall};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};

use crate::openrouter::{ChatMessage, OpenRouter};

const MAX_AGENT_ITERATIONS: usize = 5;
const ITERATION_TIMEOUT: Duration = Duration::from_secs(30);

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

/// Answers to first requests of a dialog, shared by all dialogs (as in TS)
pub type ResponseCache = Arc<Mutex<LruCache<String, String>>>;

pub fn new_response_cache(size: usize) -> ResponseCache {
    Arc::new(Mutex::new(LruCache::new(
        std::num::NonZeroUsize::new(size).expect("cache size must be positive"),
    )))
}

/// One spoken answer part with the directives collected along the way
pub struct AgentOutput {
    pub directives: Vec<DirectiveResult>,
    /// true when no function results are pending (the turn is complete)
    pub finished: bool,
    pub should_listen: bool,
    pub text: String,
}

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
        let cached = if first_request {
            self.cache.lock().await.get(&text).cloned()
        } else {
            None
        };

        for iteration in 0..MAX_AGENT_ITERATIONS {
            let response = match &cached {
                Some(cached) if iteration == 0 => cached.clone(),
                _ => self.query_llm().await?,
            };

            let structured = protocol::parse_llm_response(&response)?;
            let (mut directives, mut responses) = (Vec::new(), Vec::new());

            for call in &structured.function_calls {
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
                            responses.push((call.name.clone(), executor.call(call.clone())));
                        }
                        FunctionKind::Remote { has_response: false } => executor.spawn(call.clone()),
                    },
                }
            }

            let finished = responses.is_empty();
            {
                let mut state = self.state.lock().await;
                state.messages.push(ChatMessage::assistant(response.clone()));
                if !structured.require_more_input && structured.can_cache && state.first_request && finished
                    && cached.is_none()
                {
                    self.cache.lock().await.put(text.clone(), response.clone());
                }
                state.first_request = false;
            }

            tracing::info!("agent {}: '{}'", if finished { "finished" } else { "answered a part" }, structured.text);
            let part = AgentOutput {
                directives,
                finished,
                should_listen: structured.require_more_input,
                text: structured.text,
            };
            // nobody is left to take the answer (the dialog was dropped): stop
            if outputs.send(part).is_err() || finished {
                return Ok(());
            }

            let results = futures_util::future::join_all(
                responses.into_iter().map(|(name, result)| async move { format!("{name}: {}", result.await) }),
            )
            .await;
            self.state.lock().await.messages.push(ChatMessage::system(results.join("\n")));
        }

        tracing::warn!("agent reached the iteration limit");
        let _ = outputs.send(AgentOutput {
            directives: Vec::new(),
            finished: true,
            should_listen: false,
            text: String::new(),
        });
        Ok(())
    }

    async fn query_llm(self: &Arc<Self>) -> Result<String> {
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
                response.push_str(&delta?.0);
            }
            Ok::<(), anyhow::Error>(())
        };
        tokio::time::timeout(ITERATION_TIMEOUT, read).await??;
        tracing::info!("LLM answer: {response}");
        Ok(response)
    }
}
