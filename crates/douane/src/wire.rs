//! Speaking the other dialect.
//!
//! douane's whole surface is the Anthropic Messages API: that is what agents
//! send, what the router matches on, what the scrubber reads and what the tap
//! counts. Some upstreams only sell the OpenAI chat-completions shape, and a few
//! sell both but have let the Anthropic one rot. crof.ai is the live example:
//! `crof.ai/v1` answers 200 with a real key while `anthropic.nahcrof.com` has
//! been 502 since 2026-08-01.
//!
//! So a provider may declare `wire = "openai"`, and this module translates in
//! both directions. **The translation happens at the edges of `forward` and
//! nowhere else.** Going out, the body is rewritten just before it is sent.
//! Coming back, the response is rewritten into the Anthropic shape *before* the
//! tap sees it, so usage accounting, the audit log and the dashboard all
//! keep working on the one shape they were written for. Nothing downstream of
//! this file knows a second dialect exists.
//!
//! What is deliberately not translated:
//!
//! - **`reasoning_content`.** DeepSeek and friends return their chain of thought
//!   in that field. Anthropic's equivalent is a `thinking` block, and a real one
//!   carries a signature the next turn is expected to send back. Forging an
//!   unsigned one produces a conversation that dies on its second message, which
//!   is worse than losing text nobody reads. Dropped.
//! - **Images and documents.** No upstream reached this way serves them today,
//!   and guessing at a mapping that nothing exercises is how silent corruption
//!   gets shipped. A request carrying one is refused rather than mangled.
//! - **Prompt caching.** There is no OpenAI request field for it. Cache *reads*
//!   are reported back when the upstream volunteers `cached_tokens`, so a
//!   provider that caches on its own is still counted; douane just cannot ask.

use serde_json::{Map, Value, json};

/// Which dialect an upstream speaks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Wire {
    /// `/v1/messages`. The default, and what douane itself serves.
    #[default]
    Anthropic,
    /// `/chat/completions`, appended to `base_url` as written. Unlike the
    /// Anthropic form, an OpenAI `base_url` carries its own version segment
    /// (`https://crof.ai/v1`), because that is how those sites publish it.
    Openai,
}

impl Wire {
    /// The path this dialect's endpoint lives at, appended to `base_url`.
    pub fn path(self) -> &'static str {
        match self {
            Self::Anthropic => "/v1/messages",
            Self::Openai => "/chat/completions",
        }
    }

    /// Where this dialect publishes its catalogue, appended to `base_url` the
    /// same way [`Self::path`] is. Both dialects answer the same envelope
    /// (`{"data": [{"id": ...}]}`), which is why one parser reads either.
    pub fn models_path(self) -> &'static str {
        match self {
            Self::Anthropic => "/v1/models",
            Self::Openai => "/models",
        }
    }

    /// What this dialect is called where an upstream names it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Openai => "openai",
        }
    }

    /// Whether a model advertising these endpoint types can be reached through
    /// this dialect. An empty list means the upstream said nothing, and
    /// silence is not a refusal: the model stays servable.
    ///
    /// `openai-response` does not count as OpenAI here. It is the Responses
    /// API, and douane's OpenAI path posts to `/chat/completions`, so a model
    /// offering only that one answers 404 on every call douane knows how to
    /// make.
    pub fn speaks(self, endpoints: &[String]) -> bool {
        if endpoints.is_empty() {
            return true;
        }
        endpoints.iter().any(|e| match self {
            Self::Anthropic => e.eq_ignore_ascii_case("anthropic"),
            Self::Openai => e.eq_ignore_ascii_case("openai"),
        })
    }
}

/// A request douane will not translate rather than translate wrongly.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Untranslatable(pub String);

/// Anthropic Messages request to OpenAI chat completions.
///
/// `model` is already whatever the route said, since the caller rewrites it
/// before handing the body over.
pub fn request_to_openai(body: &[u8]) -> Result<Vec<u8>, Untranslatable> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| Untranslatable(format!("body is not JSON: {e}")))?;

    let mut out = Map::new();
    for key in ["model", "temperature", "top_p", "stream"] {
        if let Some(x) = v.get(key) {
            out.insert(key.to_owned(), x.clone());
        }
    }
    // Anthropic requires max_tokens, OpenAI treats it as optional. Carrying it
    // over matters: without it some upstreams answer until their own ceiling and
    // bill the difference.
    if let Some(x) = v.get("max_tokens") {
        out.insert("max_tokens".to_owned(), x.clone());
    }
    if let Some(x) = v.get("stop_sequences") {
        out.insert("stop".to_owned(), x.clone());
    }

    let mut messages = Vec::new();
    // Anthropic keeps the system prompt out of the message list; OpenAI wants it
    // as the first message. Both the string and the block-array form appear in
    // the wild, and Claude Code sends the array.
    if let Some(sys) = v.get("system")
        && let Some(text) = flatten_text(sys)
        && !text.is_empty()
    {
        messages.push(json!({"role": "system", "content": text}));
    }

    let turns = v
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| Untranslatable("no `messages` array".into()))?;
    for turn in turns {
        translate_turn(turn, &mut messages)?;
    }
    out.insert("messages".to_owned(), Value::Array(messages));

    if let Some(tools) = v.get("tools").and_then(Value::as_array) {
        let translated: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.get("name").cloned().unwrap_or(Value::Null),
                        "description": t.get("description").cloned().unwrap_or(Value::Null),
                        "parameters": t.get("input_schema").cloned().unwrap_or(json!({})),
                    }
                })
            })
            .collect();
        if !translated.is_empty() {
            out.insert("tools".to_owned(), Value::Array(translated));
        }
    }
    if let Some(tc) = v.get("tool_choice") {
        out.insert("tool_choice".to_owned(), tool_choice_to_openai(tc));
    }

    // Without this a streamed OpenAI response carries no usage at all, and the
    // call would land in the ledger with tokens unknown. It is the one field
    // that has to be added rather than mapped.
    if v.get("stream").and_then(Value::as_bool) == Some(true) {
        out.insert("stream_options".to_owned(), json!({"include_usage": true}));
    }

    serde_json::to_vec(&Value::Object(out))
        .map_err(|e| Untranslatable(format!("cannot serialise: {e}")))
}

/// One Anthropic turn becomes one or more OpenAI messages.
///
/// The count differs because Anthropic packs tool results into a user turn as
/// content blocks, while OpenAI gives each one its own `role: "tool"` message.
fn translate_turn(turn: &Value, out: &mut Vec<Value>) -> Result<(), Untranslatable> {
    let role = turn.get("role").and_then(Value::as_str).unwrap_or("user");
    let content = turn.get("content").unwrap_or(&Value::Null);

    let Some(blocks) = content.as_array() else {
        // The plain-string form, which needs nothing doing to it.
        out.push(json!({"role": role, "content": content.clone()}));
        return Ok(());
    };

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut results = Vec::new();

    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
            Some("tool_use") => tool_calls.push(json!({
                "id": b.get("id").cloned().unwrap_or(Value::Null),
                "type": "function",
                "function": {
                    "name": b.get("name").cloned().unwrap_or(Value::Null),
                    // OpenAI carries arguments as a JSON *string*, Anthropic as an
                    // object. A nested object here is silently ignored by most
                    // upstreams, which looks like the model calling a tool with no
                    // arguments at all.
                    "arguments": Value::String(
                        serde_json::to_string(b.get("input").unwrap_or(&json!({})))
                            .unwrap_or_else(|_| "{}".into())
                    ),
                }
            })),
            Some("tool_result") => results.push(json!({
                "role": "tool",
                "tool_call_id": b.get("tool_use_id").cloned().unwrap_or(Value::Null),
                "content": flatten_text(b.get("content").unwrap_or(&Value::Null))
                    .unwrap_or_default(),
            })),
            // Signed thinking from a previous turn. Dropping it is safe in this
            // direction: it is context the upstream never produced and cannot
            // verify.
            Some("thinking") | Some("redacted_thinking") => {}
            Some(other) => {
                return Err(Untranslatable(format!(
                    "this upstream speaks the OpenAI shape, which has no equivalent for a `{other}` content block"
                )));
            }
            None => {}
        }
    }

    // A turn that was nothing but tool results has no message of its own: the
    // results are the whole turn. Emitting an empty user message alongside them
    // makes some upstreams answer as though the user had said nothing.
    let empty = text.is_empty() && tool_calls.is_empty();
    if !(empty && !results.is_empty()) {
        let mut m = Map::new();
        m.insert("role".to_owned(), Value::String(role.to_owned()));
        m.insert("content".to_owned(), Value::String(text));
        if !tool_calls.is_empty() {
            m.insert("tool_calls".to_owned(), Value::Array(tool_calls));
        }
        out.push(Value::Object(m));
    }
    out.extend(results);
    Ok(())
}

fn tool_choice_to_openai(tc: &Value) -> Value {
    match tc.get("type").and_then(Value::as_str) {
        Some("auto") => Value::String("auto".into()),
        Some("any") => Value::String("required".into()),
        Some("none") => Value::String("none".into()),
        Some("tool") => json!({
            "type": "function",
            "function": {"name": tc.get("name").cloned().unwrap_or(Value::Null)}
        }),
        _ => Value::String("auto".into()),
    }
}

/// Anthropic's "a string, or an array of blocks" shape, reduced to text.
fn flatten_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => Some(
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

/// An OpenAI usage object read into the Anthropic one.
///
/// `prompt_tokens` counts cached tokens as well on every upstream seen so far,
/// while Anthropic reports them apart, so the cached share is subtracted. Getting
/// this backwards double-counts the cheap tokens at the full input rate.
fn usage_to_anthropic(u: &Value) -> Value {
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let cached = u
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    json!({
        "input_tokens": n("prompt_tokens").saturating_sub(cached),
        "output_tokens": n("completion_tokens"),
        "cache_read_input_tokens": cached,
    })
}

fn stop_reason(finish: Option<&str>) -> &'static str {
    match finish {
        Some("length") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        Some("content_filter") => "refusal",
        _ => "end_turn",
    }
}

/// A buffered OpenAI response read back into the Anthropic shape.
pub fn response_to_anthropic(body: &[u8], model: &str) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(body).ok()?;
    // An error body is left exactly as it came. The shapes differ, but a caller
    // reading a translated error would be reading douane's guess at what went
    // wrong instead of what the upstream said.
    if v.get("error").is_some() || v.get("choices").is_none() {
        return None;
    }
    let choice = v.get("choices")?.get(0)?;
    let message = choice.get("message")?;

    let mut content = Vec::new();
    if let Some(t) = message.get("content").and_then(Value::as_str)
        && !t.is_empty()
    {
        content.push(json!({"type": "text", "text": t}));
    }
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let f = call.get("function");
        let args = f
            .and_then(|f| f.get("arguments"))
            .and_then(Value::as_str)
            .unwrap_or("{}");
        content.push(json!({
            "type": "tool_use",
            "id": call.get("id").cloned().unwrap_or(Value::Null),
            "name": f.and_then(|f| f.get("name")).cloned().unwrap_or(Value::Null),
            // Arguments arrive as a JSON string. A model that emitted broken JSON
            // gets it handed on as an empty object rather than taking the whole
            // response down.
            "input": serde_json::from_str::<Value>(args).unwrap_or_else(|_| json!({})),
        }));
    }

    let out = json!({
        "id": v.get("id").cloned().unwrap_or(Value::Null),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason(choice.get("finish_reason").and_then(Value::as_str)),
        "stop_sequence": Value::Null,
        // Same rule as the streamed close: an upstream that reports nothing still
        // owes the client the two counters, zeroed.
        "usage": v
            .get("usage")
            .map_or_else(|| json!({"input_tokens": 0, "output_tokens": 0}), usage_to_anthropic),
    });
    serde_json::to_vec(&out).ok()
}

/// Turns an OpenAI SSE stream into an Anthropic one, frame by frame.
///
/// Stateful because the two event vocabularies do not line up. OpenAI opens with
/// a delta and closes with `[DONE]`; Anthropic brackets the answer with
/// `message_start` / `message_stop` and every content block with its own
/// start and stop. So the first delta has to synthesise the opening events, and
/// the end of the stream has to close whatever was left open.
///
/// **`message_start` goes out with zeroed usage on purpose.** OpenAI only reports
/// counts in its final frame, so the real numbers ride in `message_delta` at the
/// end. The tap absorbs usage from both events and the later one wins, so the
/// ledger is right; a client that reads `message_start` alone sees zeros, which
/// is the one visible cost of this translation.
/// What an open content block accepts. Anthropic types its blocks and its
/// deltas together, so the wrong pairing is not a formatting slip: a client
/// indexing parts by block has nowhere to put the delta and drops the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Tool,
}

pub struct StreamTranslator {
    model: String,
    partial: String,
    started: bool,
    /// The open content block: its index, and what it accepts. Anthropic numbers
    /// blocks within a message, expects them closed in order, and takes only the
    /// one delta kind a block was opened for.
    open: Option<(usize, BlockKind)>,
    next_index: usize,
    finish: Option<String>,
    usage: Option<Value>,
    /// What the upstream said went wrong, when it said anything at all. Kept so
    /// a stream that produced no answer can say why instead of ending quietly.
    upstream_error: Option<String>,
    done: bool,
}

impl StreamTranslator {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_owned(),
            partial: String::new(),
            started: false,
            open: None,
            next_index: 0,
            finish: None,
            usage: None,
            upstream_error: None,
            done: false,
        }
    }

    /// Feed one upstream chunk, get back the Anthropic frames it produced.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        let Ok(text) = std::str::from_utf8(chunk) else {
            return Vec::new();
        };
        self.partial.push_str(text);
        let Some(cut) = self.partial.rfind('\n') else {
            // A frame longer than any real one is not one. Cap rather than let a
            // malformed upstream grow this without end.
            if self.partial.len() > 1_000_000 {
                self.partial.clear();
            }
            return Vec::new();
        };
        let complete: String = self.partial.drain(..=cut).collect();

        let mut out = String::new();
        for line in complete.lines() {
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload == "[DONE]" {
                self.close(&mut out);
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(payload) else {
                continue;
            };
            self.frame(&v, &mut out);
        }
        out.into_bytes()
    }

    /// Called when the upstream stream ends, whether or not `[DONE]` arrived.
    /// A truncated stream still has to be closed or the client waits forever.
    pub fn finish(&mut self) -> Vec<u8> {
        let mut out = String::new();
        self.close(&mut out);
        out.into_bytes()
    }

    fn frame(&mut self, v: &Value, out: &mut String) {
        if let Some(u) = v.get("usage")
            && !u.is_null()
        {
            self.usage = Some(usage_to_anthropic(u));
        }

        // An OpenAI upstream reports a mid-stream failure as a bare object with no
        // `choices`. Dropping that frame is how a failed call reaches the client
        // looking like a valid empty answer, so the reason is kept for `close`.
        if self.upstream_error.is_none()
            && let Some(e) = v.get("error")
            && !e.is_null()
        {
            self.upstream_error = Some(match e.get("message").and_then(Value::as_str) {
                Some(m) => m.to_owned(),
                None => e.to_string(),
            });
        }

        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
            return;
        };
        if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(f.to_owned());
        }
        let Some(delta) = choice.get("delta") else {
            return;
        };

        if !self.started {
            self.started = true;
            emit(
                out,
                "message_start",
                &json!({
                    "type": "message_start",
                    "message": {
                        "id": v.get("id").cloned().unwrap_or(Value::Null),
                        "type": "message",
                        "role": "assistant",
                        "model": self.model,
                        "content": [],
                        "stop_reason": Value::Null,
                        "stop_sequence": Value::Null,
                        "usage": {"input_tokens": 0, "output_tokens": 0},
                    }
                }),
            );
        }

        if let Some(t) = delta.get("content").and_then(Value::as_str)
            && !t.is_empty()
        {
            // Text arriving after a tool call has to start its own block. Sending
            // it at the tool's index is a text delta on a block the client opened
            // as a tool call, and a client tracking parts by index has nothing to
            // append it to: the AI SDK answers `text part <n> not found` and drops
            // the message, which reads as a model that stopped for no reason.
            if !matches!(self.open, Some((_, BlockKind::Text))) {
                self.close_block(out);
                self.open_block(out, BlockKind::Text, &json!({"type": "text", "text": ""}));
            }
            emit(
                out,
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": self.open.map_or(0, |(i, _)| i),
                    "delta": {"type": "text_delta", "text": t},
                }),
            );
        }

        for call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            // A tool call announces its name once, then streams its arguments.
            // The name is what says a new block has begun.
            let f = call.get("function");
            if let Some(name) = f.and_then(|f| f.get("name")).and_then(Value::as_str) {
                self.close_block(out);
                self.open_block(
                    out,
                    BlockKind::Tool,
                    &json!({
                        "type": "tool_use",
                        "id": call.get("id").cloned().unwrap_or(Value::Null),
                        "name": name,
                        "input": {},
                    }),
                );
            }
            if let Some(args) = f.and_then(|f| f.get("arguments")).and_then(Value::as_str)
                && !args.is_empty()
            {
                // Arguments before the name that opens their block have nothing to
                // attach to, and a text block is the wrong home for them. Anthropic
                // has no way to say "a tool call whose name is still coming", so
                // they are dropped rather than sent somewhere they corrupt.
                let Some((index, BlockKind::Tool)) = self.open else {
                    tracing::warn!(
                        model = %self.model,
                        "tool arguments arrived before the name that opens their block"
                    );
                    continue;
                };
                emit(
                    out,
                    "content_block_delta",
                    &json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "input_json_delta", "partial_json": args},
                    }),
                );
            }
        }
    }

    fn open_block(&mut self, out: &mut String, kind: BlockKind, block: &Value) {
        let index = self.next_index;
        self.next_index += 1;
        self.open = Some((index, kind));
        emit(
            out,
            "content_block_start",
            &json!({"type": "content_block_start", "index": index, "content_block": block}),
        );
    }

    fn close_block(&mut self, out: &mut String) {
        if let Some((index, _)) = self.open.take() {
            emit(
                out,
                "content_block_stop",
                &json!({"type": "content_block_stop", "index": index}),
            );
        }
    }

    fn close(&mut self, out: &mut String) {
        if self.done {
            return;
        }
        self.done = true;
        // An upstream that never sent a single frame produced no answer, and it
        // used to be closed here as a well-formed empty message. That tells the
        // client the model chose to say nothing, so an agent reads a finished turn
        // and stops mid-task with no reason given and nothing to retry on. An
        // error event is what actually happened, and it is a shape the Anthropic
        // protocol already carries mid-stream.
        if !self.started {
            let message = self.upstream_error.clone().unwrap_or_else(|| {
                "the upstream closed the stream without sending a single frame".to_owned()
            });
            tracing::warn!(
                model = %self.model,
                reason = %message,
                "a translated stream produced no answer at all"
            );
            emit(
                out,
                "error",
                &json!({
                    "type": "error",
                    "error": {"type": "api_error", "message": message},
                }),
            );
            return;
        }
        self.close_block(out);
        // `usage` is not optional on this event: the Anthropic schema requires it,
        // and a client that validates the stream rejects the whole response when it
        // is missing. Every stream reaching this point without a final usage frame
        // is affected, an upstream cut off mid-answer included, since that lands
        // here too. Zeros go out rather than nothing.
        let delta = json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason(self.finish.as_deref()), "stop_sequence": Value::Null},
            "usage": self
                .usage
                .clone()
                .unwrap_or_else(|| json!({"input_tokens": 0, "output_tokens": 0})),
        });
        emit(out, "message_delta", &delta);
        emit(out, "message_stop", &json!({"type": "message_stop"}));
    }
}

/// Wraps an upstream response so that whatever comes out speaks Anthropic.
///
/// Three shapes go through here. An Anthropic upstream passes through untouched,
/// which is the case that must stay free. A streamed OpenAI one is translated
/// frame by frame, so a long generation still costs a few hundred bytes of state
/// rather than a copy of the answer. A buffered OpenAI one has to be held whole,
/// because the object cannot be rewritten until its last brace has arrived, and
/// is capped for the same reason the tap caps its own buffer.
/// Boxed and pinned because `unfold` produces a self-referential future, and the
/// tap downstream needs a stream it can poll without pinning it itself.
pub fn translate_stream<S, E>(
    inner: S,
    wire: Wire,
    sse: bool,
    model: String,
) -> std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send>>
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    /// The most of a buffered body worth rewriting. Past this it is not a body
    /// whose `choices` are going to be found, and holding it would let one slow
    /// upstream pin megabytes per call in flight.
    const JSON_CAP: usize = 4 * 1024 * 1024;

    enum How {
        /// Anthropic upstream, or an error status: hand the bytes on as they are.
        Passthrough,
        Sse(StreamTranslator),
        Json(Vec<u8>),
    }

    struct St<S> {
        inner: S,
        how: How,
        model: String,
        /// Set once the upstream has run out, so the closing frames are emitted
        /// exactly once and the stream then ends.
        drained: bool,
    }

    let how = match (wire, sse) {
        (Wire::Anthropic, _) => How::Passthrough,
        (Wire::Openai, true) => How::Sse(StreamTranslator::new(&model)),
        (Wire::Openai, false) => How::Json(Vec::new()),
    };

    Box::pin(futures_util::stream::unfold(
        St {
            inner,
            how,
            model,
            drained: false,
        },
        |mut st| async move {
            use futures_util::StreamExt as _;
            if st.drained {
                return None;
            }
            match st.inner.next().await {
                Some(Ok(chunk)) => match &mut st.how {
                    How::Passthrough => Some((Ok(chunk), st)),
                    How::Sse(t) => {
                        let out = t.push(&chunk);
                        // A chunk that produced no complete frame yields an empty
                        // one rather than ending the stream: the bytes are held
                        // for the next round.
                        Some((Ok(bytes::Bytes::from(out)), st))
                    }
                    How::Json(buf) => {
                        if buf.len() + chunk.len() <= JSON_CAP {
                            buf.extend_from_slice(&chunk);
                        }
                        Some((Ok(bytes::Bytes::new()), st))
                    }
                },
                Some(Err(e)) => {
                    st.drained = true;
                    Some((Err(std::io::Error::other(e)), st))
                }
                None => {
                    st.drained = true;
                    match &mut st.how {
                        How::Passthrough => None,
                        How::Sse(t) => Some((Ok(bytes::Bytes::from(t.finish())), st)),
                        How::Json(buf) => {
                            let body = std::mem::take(buf);
                            // An error body, or anything else that is not a chat
                            // completion, goes back exactly as it was written.
                            // Translating it would report douane's guess at the
                            // failure instead of the upstream's own words.
                            let out = response_to_anthropic(&body, &st.model).unwrap_or(body);
                            Some((Ok(bytes::Bytes::from(out)), st))
                        }
                    }
                }
            }
        },
    ))
}

fn emit(out: &mut String, event: &str, data: &Value) {
    out.push_str("event: ");
    out.push_str(event);
    out.push_str("\ndata: ");
    out.push_str(&data.to_string());
    out.push_str("\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn openai(body: &str) -> Value {
        serde_json::from_slice(&request_to_openai(body.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn the_system_prompt_becomes_the_first_message() {
        let v = openai(
            r#"{"model":"m","max_tokens":8,
                "system":[{"type":"text","text":"be brief"}],
                "messages":[{"role":"user","content":"hi"}]}"#,
        );
        assert_eq!(v["messages"][0]["role"], "system");
        assert_eq!(v["messages"][0]["content"], "be brief");
        assert_eq!(v["messages"][1]["role"], "user");
        assert_eq!(v["max_tokens"], 8);
    }

    #[test]
    fn stop_sequences_take_the_name_the_other_side_uses() {
        let v = openai(
            r#"{"model":"m","stop_sequences":["END"],"messages":[{"role":"user","content":"hi"}]}"#,
        );
        assert_eq!(v["stop"][0], "END");
        assert!(v.get("stop_sequences").is_none());
    }

    #[test]
    fn a_streamed_request_asks_for_the_counts_it_will_be_billed_for() {
        let v =
            openai(r#"{"model":"m","stream":true,"messages":[{"role":"user","content":"hi"}]}"#);
        assert_eq!(v["stream_options"]["include_usage"], true);

        let v = openai(r#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#);
        assert!(
            v.get("stream_options").is_none(),
            "a buffered call carries usage in its body and needs no flag"
        );
    }

    #[test]
    fn a_tool_call_arrives_with_its_arguments_as_a_string() {
        let v = openai(
            r#"{"model":"m","messages":[
                {"role":"assistant","content":[
                  {"type":"tool_use","id":"t1","name":"grep","input":{"q":"x"}}]}]}"#,
        );
        let call = &v["messages"][0]["tool_calls"][0];
        assert_eq!(call["function"]["name"], "grep");
        // A nested object here reads as "called with no arguments" upstream.
        assert_eq!(call["function"]["arguments"], r#"{"q":"x"}"#);
    }

    #[test]
    fn a_tool_result_becomes_a_message_of_its_own() {
        let v = openai(
            r#"{"model":"m","messages":[
                {"role":"user","content":[
                  {"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"42"}]}]}]}"#,
        );
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(
            msgs.len(),
            1,
            "the empty user turn is not sent, only the result"
        );
        assert_eq!(msgs[0]["role"], "tool");
        assert_eq!(msgs[0]["tool_call_id"], "t1");
        assert_eq!(msgs[0]["content"], "42");
    }

    #[test]
    fn a_block_with_no_equivalent_is_refused_rather_than_dropped() {
        let e = request_to_openai(
            br#"{"model":"m","messages":[{"role":"user","content":[{"type":"image","source":{}}]}]}"#,
        )
        .unwrap_err();
        assert!(e.0.contains("image"), "{}", e.0);
    }

    #[test]
    fn a_tool_declaration_moves_under_function() {
        let v = openai(
            r#"{"model":"m","messages":[{"role":"user","content":"hi"}],
                "tools":[{"name":"ls","description":"list","input_schema":{"type":"object"}}],
                "tool_choice":{"type":"any"}}"#,
        );
        assert_eq!(v["tools"][0]["type"], "function");
        assert_eq!(v["tools"][0]["function"]["name"], "ls");
        assert_eq!(v["tools"][0]["function"]["parameters"]["type"], "object");
        assert_eq!(v["tool_choice"], "required");
    }

    #[test]
    fn a_buffered_answer_comes_back_in_the_shape_the_tap_reads() {
        let body = br#"{"id":"c1","choices":[{"index":0,"finish_reason":"stop",
            "message":{"role":"assistant","content":"hello"}}],
            "usage":{"prompt_tokens":1000,"completion_tokens":7,
                     "prompt_tokens_details":{"cached_tokens":400}}}"#;
        let v: Value =
            serde_json::from_slice(&response_to_anthropic(body, "deepseek-v4-flash").unwrap())
                .unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["model"], "deepseek-v4-flash");
        assert_eq!(v["content"][0]["text"], "hello");
        assert_eq!(v["stop_reason"], "end_turn");
        // 1000 prompt tokens of which 400 were cached: 600 fresh, not 1000, or
        // the cheap tokens get billed twice at the full rate.
        assert_eq!(v["usage"]["input_tokens"], 600);
        assert_eq!(v["usage"]["cache_read_input_tokens"], 400);
        assert_eq!(v["usage"]["output_tokens"], 7);
    }

    #[test]
    fn an_upstream_error_is_handed_back_as_it_was_written() {
        let body = br#"{"error":{"message":"no credit","type":"insufficient_quota"}}"#;
        assert!(
            response_to_anthropic(body, "m").is_none(),
            "translating an error would report douane's guess instead of the upstream's answer"
        );
    }

    #[test]
    fn reasoning_text_never_becomes_a_thinking_block() {
        let body = br#"{"id":"c1","choices":[{"finish_reason":"stop","message":
            {"role":"assistant","content":"ok","reasoning_content":"let me think"}}]}"#;
        let v: Value = serde_json::from_slice(&response_to_anthropic(body, "m").unwrap()).unwrap();
        let blocks = v["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "text");
    }

    /// Collect every `data:` payload a translator produced, in order.
    fn frames(out: &[u8]) -> Vec<Value> {
        std::str::from_utf8(out)
            .unwrap()
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .map(|p| serde_json::from_str(p).unwrap())
            .collect()
    }

    #[test]
    fn a_text_stream_gets_the_events_an_anthropic_client_expects() {
        let mut t = StreamTranslator::new("deepseek-v4-flash");
        let mut all = Vec::new();
        all.extend(
            t.push(b"data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"He\"}}]}\n"),
        );
        all.extend(t.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"llo\"}}]}\n"));
        all.extend(t.push(
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3}}\n",
        ));
        all.extend(t.push(b"data: [DONE]\n"));

        let f = frames(&all);
        let types: Vec<&str> = f.iter().map(|x| x["type"].as_str().unwrap_or("")).collect();
        assert_eq!(
            types,
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );

        // The tap reads counts off `message_delta`, so this is where the ledger
        // gets its numbers on a translated stream.
        let last_delta = f
            .iter()
            .find(|x| x["type"] == "message_delta")
            .unwrap()
            .clone();
        assert_eq!(last_delta["usage"]["input_tokens"], 12);
        assert_eq!(last_delta["usage"]["output_tokens"], 3);
        assert_eq!(last_delta["delta"]["stop_reason"], "end_turn");
    }

    /// `stream_options.include_usage` is a request, not a guarantee: an upstream
    /// can finish without ever sending counts. Leaving `usage` off the closing
    /// event fails Anthropic schema validation client side, which reads as a
    /// broken model rather than a silent upstream.
    /// Five calls on one route came back 200 with an empty body and a 90 s
    /// average, which the client read as a finished turn saying nothing and an
    /// agent read as permission to stop. Nothing was logged and nothing was
    /// retryable.
    #[test]
    fn a_stream_that_produced_nothing_says_so_instead_of_closing_empty() {
        let mut t = StreamTranslator::new("m");
        let all = t.finish();

        let f = frames(&all);
        let types: Vec<&str> = f.iter().map(|x| x["type"].as_str().unwrap_or("")).collect();
        assert_eq!(types, ["error"], "{types:?}");
        assert_eq!(f[0]["error"]["type"], "api_error");
    }

    /// A failure the upstream did explain reaches the client in its own words,
    /// rather than as douane's guess at what went wrong.
    #[test]
    fn an_upstream_that_errors_mid_stream_hands_its_reason_on() {
        let mut t = StreamTranslator::new("m");
        let mut all = Vec::new();
        all.extend(
            t.push(b"data: {\"error\":{\"message\":\"rate limited\",\"type\":\"rate_limit\"}}\n"),
        );
        all.extend(t.push(b"data: [DONE]\n"));

        let f = frames(&all);
        assert_eq!(f[0]["type"], "error");
        assert_eq!(f[0]["error"]["message"], "rate limited");
    }

    #[test]
    fn a_stream_whose_upstream_never_reports_usage_still_closes_with_zeros() {
        let mut t = StreamTranslator::new("ox-alpha-free");
        let mut all = Vec::new();
        all.extend(
            t.push(b"data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n"),
        );
        all.extend(t.push(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n"));
        all.extend(t.push(b"data: [DONE]\n"));

        let f = frames(&all);
        let last = f.iter().find(|x| x["type"] == "message_delta").unwrap();
        assert_eq!(last["usage"]["input_tokens"], 0);
        assert_eq!(last["usage"]["output_tokens"], 0);
        assert_eq!(last["delta"]["stop_reason"], "end_turn");
    }

    #[test]
    fn a_translated_stream_is_countable_by_the_tap_that_reads_anthropic() {
        let mut t = StreamTranslator::new("m");
        let mut all = Vec::new();
        all.extend(
            t.push(b"data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n"),
        );
        all.extend(t.push(
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":900,\"completion_tokens\":40,\"prompt_tokens_details\":{\"cached_tokens\":100}}}\n",
        ));
        all.extend(t.push(b"data: [DONE]\n"));

        let mut tap = crate::usage::Tap::new(crate::usage::Mode::Sse);
        tap.feed(&all);
        let u = tap.usage();
        assert_eq!(u.input_tokens, Some(800));
        assert_eq!(u.cache_read_tokens, Some(100));
        assert_eq!(u.output_tokens, Some(40));
    }

    #[test]
    fn a_streamed_tool_call_opens_and_closes_its_own_block() {
        let mut t = StreamTranslator::new("m");
        let mut all = Vec::new();
        all.extend(t.push(
            b"data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"t1\",\"function\":{\"name\":\"ls\",\"arguments\":\"\"}}]}}]}\n",
        ));
        all.extend(t.push(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"{\\\"p\\\":1}\"}}]}}]}\n",
        ));
        all.extend(
            t.push(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n"),
        );
        all.extend(t.push(b"data: [DONE]\n"));

        let f = frames(&all);
        let start = f
            .iter()
            .find(|x| x["type"] == "content_block_start")
            .unwrap();
        assert_eq!(start["content_block"]["type"], "tool_use");
        assert_eq!(start["content_block"]["name"], "ls");
        let delta = f
            .iter()
            .find(|x| x["delta"]["type"] == "input_json_delta")
            .unwrap();
        assert_eq!(delta["delta"]["partial_json"], r#"{"p":1}"#);
        assert_eq!(
            f.iter().find(|x| x["type"] == "message_delta").unwrap()["delta"]["stop_reason"],
            "tool_use"
        );
    }

    /// Text, a tool call, then more text. The last stretch used to go out as a
    /// text delta on the tool call's index, because the open block carried no
    /// kind. The AI SDK indexes parts by that index, finds a tool call where a
    /// text part should be, answers `text part 1 not found` and drops the whole
    /// message, which reads as an agent that stopped mid-edit.
    #[test]
    fn text_after_a_tool_call_starts_its_own_block() {
        let mut t = StreamTranslator::new("m");
        let mut all = Vec::new();
        all.extend(t.push(b"data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"before\"}}]}\n"));
        all.extend(t.push(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"t1\",\"function\":{\"name\":\"ls\",\"arguments\":\"{}\"}}]}}]}\n",
        ));
        all.extend(t.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"after\"}}]}\n"));
        all.extend(
            t.push(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n"),
        );
        all.extend(t.push(b"data: [DONE]\n"));

        let f = frames(&all);
        // Every delta lands on a block opened for the kind it carries, and every
        // block that opened also closed.
        let mut kind = std::collections::HashMap::new();
        let mut open = std::collections::HashSet::new();
        for x in &f {
            let i = x["index"].as_u64();
            match x["type"].as_str() {
                Some("content_block_start") => {
                    kind.insert(i, x["content_block"]["type"].as_str().unwrap().to_owned());
                    open.insert(i);
                }
                Some("content_block_stop") => {
                    open.remove(&i);
                }
                Some("content_block_delta") => {
                    let want = match x["delta"]["type"].as_str() {
                        Some("text_delta") => "text",
                        _ => "tool_use",
                    };
                    assert_eq!(kind.get(&i).map(String::as_str), Some(want), "index {i:?}");
                }
                _ => {}
            }
        }
        assert!(open.is_empty(), "a block left open hangs the client");
        assert_eq!(kind.len(), 3, "text, tool call, then text again: {kind:?}");
    }

    /// Arguments cannot be attached before the name that opens their block, and
    /// a text block is the wrong home for them.
    #[test]
    fn tool_arguments_with_no_block_open_are_dropped_not_misfiled() {
        let mut t = StreamTranslator::new("m");
        let mut all = Vec::new();
        all.extend(t.push(b"data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n"));
        all.extend(t.push(
            b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"{\\\"p\\\":1}\"}}]}}]}\n",
        ));
        all.extend(t.push(b"data: [DONE]\n"));

        let f = frames(&all);
        assert!(
            !f.iter()
                .any(|x| x["delta"]["type"] == "input_json_delta"),
            "arguments on a text block corrupt the message: {f:?}"
        );
    }

    #[test]
    fn a_stream_cut_off_early_is_still_closed() {
        let mut t = StreamTranslator::new("m");
        let mut all = Vec::new();
        all.extend(
            t.push(b"data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n"),
        );
        // No finish_reason, no [DONE]: the upstream simply stopped.
        all.extend(t.finish());

        let f = frames(&all);
        let types: Vec<&str> = f.iter().map(|x| x["type"].as_str().unwrap_or("")).collect();
        assert_eq!(types.last(), Some(&"message_stop"), "{types:?}");
        assert!(
            types.contains(&"content_block_stop"),
            "an open block left open hangs the client: {types:?}"
        );
    }

    #[test]
    fn done_arriving_twice_does_not_close_the_message_twice() {
        let mut t = StreamTranslator::new("m");
        let mut all = t.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n");
        all.extend(t.push(b"data: [DONE]\n"));
        all.extend(t.finish());
        let stops = frames(&all)
            .iter()
            .filter(|f| f["type"] == "message_stop")
            .count();
        assert_eq!(stops, 1);
    }

    #[test]
    fn a_frame_split_across_chunks_survives_the_boundary() {
        let mut t = StreamTranslator::new("m");
        let first = t.push(b"data: {\"choices\":[{\"delta\":{\"conte");
        assert!(first.is_empty(), "half a frame is not a frame");
        let second = t.push(b"nt\":\"hi\"}}]}\n");
        let f = frames(&second);
        assert_eq!(
            f.iter()
                .find(|x| x["type"] == "content_block_delta")
                .unwrap()["delta"]["text"],
            "hi"
        );
    }

    #[test]
    fn the_path_differs_and_nothing_else_does() {
        assert_eq!(Wire::Anthropic.path(), "/v1/messages");
        assert_eq!(Wire::Openai.path(), "/chat/completions");
        assert_eq!(Wire::default(), Wire::Anthropic);
    }
}
