//! Ephemeral native Gemini history, scoped to a credential, model and session.
//! Signed and hidden model parts stay in memory and are never client extensions.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashSet, VecDeque},
    io::{self, Write},
    time::{Duration, Instant},
};

const MAX_TURN_BYTES: usize = 256 * 1024;
type Fingerprint = [u8; 32];

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Scope {
    pub(crate) account: String,
    pub(crate) model: String,
    pub(crate) session: String,
}

/// A finalized native turn. Deliberately has no Debug or serialization support.
pub(crate) struct RecordedTurn {
    scope: Scope,
    context: Fingerprint,
    visible: Fingerprint,
    parts: Vec<Value>,
    call_ids: HashSet<String>,
    bytes: usize,
}

/// Call observe before converting every native event. Call abort on a transport
/// error or disconnect, and finish only after a clean stream/body completion.
pub(crate) struct Recorder {
    scope: Scope,
    context: Fingerprint,
    parts: Vec<Value>,
    call_ids: HashSet<String>,
    parts_bytes: usize,
    terminal: bool,
    invalid: bool,
}

impl Recorder {
    pub(crate) fn new(scope: Scope, outgoing_contents: Vec<Value>) -> Self {
        let mut context = history_hasher();
        for content in &outgoing_contents {
            hash_content(&mut context, content);
        }
        Self {
            scope,
            context: context.finalize().into(),
            parts: Vec::new(),
            call_ids: HashSet::new(),
            parts_bytes: 2,
            terminal: false,
            invalid: false,
        }
    }

    pub(crate) fn observe(&mut self, event: &mut Value) {
        if let Some(events) = event.as_array_mut() {
            for event in events {
                self.observe(event);
            }
            return;
        }
        if event.get("error").is_some() {
            self.abort();
        }
        let body = if event.get("response").is_some() {
            &mut event["response"]
        } else {
            event
        };
        if body.get("error").is_some() {
            self.abort();
        }
        let Some(candidate) = body
            .get_mut("candidates")
            .and_then(Value::as_array_mut)
            .and_then(|candidates| candidates.first_mut())
        else {
            return;
        };
        let terminal = candidate
            .get("finishReason")
            .or_else(|| candidate.get("finish_reason"))
            .and_then(Value::as_str)
            .is_some_and(|reason| !reason.is_empty() && reason != "FINISH_REASON_UNSPECIFIED");
        if let Some(parts) = candidate
            .get_mut("content")
            .and_then(|content| content.get_mut("parts"))
            .and_then(Value::as_array_mut)
        {
            for part in parts {
                // Uniform downstream IDs are needed even without a stable session
                // or after abandoning an oversized/incomplete recording.
                if let Some(call) = part.get_mut("functionCall").and_then(Value::as_object_mut) {
                    if !call.contains_key("id") {
                        call.insert(
                            "id".into(),
                            Value::String(format!("call_douane_{:032x}", rand::random::<u128>())),
                        );
                    }
                    if !self.invalid {
                        match call.get("id").and_then(Value::as_str) {
                            Some(id) if !id.is_empty() => {
                                if !self.call_ids.insert(id.to_owned()) {
                                    self.abort();
                                }
                            }
                            _ => self.abort(),
                        }
                    }
                }
                if !self.invalid {
                    self.push(part);
                }
            }
        }
        self.terminal |= terminal;
    }

    fn push(&mut self, part: &Value) {
        if let Some(last) = self
            .parts
            .last_mut()
            .filter(|last| mergeable_text(last, part))
        {
            let extra = serialized_size(&part["text"]) - 2;
            if self.parts_bytes.saturating_add(extra) > MAX_TURN_BYTES {
                self.abort();
                return;
            }
            if let Value::String(text) = &mut last["text"] {
                text.push_str(part["text"].as_str().unwrap());
            }
            self.parts_bytes += extra;
        } else {
            let size = serialized_size(part);
            if self
                .parts_bytes
                .saturating_add(size + usize::from(!self.parts.is_empty()))
                > MAX_TURN_BYTES
            {
                self.abort();
                return;
            }
            self.parts_bytes += size + usize::from(!self.parts.is_empty());
            self.parts.push(part.clone());
        }
        if self.parts_bytes > MAX_TURN_BYTES {
            self.abort();
        }
    }

    pub(crate) fn abort(&mut self) {
        self.invalid = true;
        self.parts.clear();
        self.call_ids.clear();
    }

    pub(crate) fn finish(self) -> Option<RecordedTurn> {
        if self.invalid
            || !self.terminal
            || self.parts.is_empty()
            || self.scope.session.trim().is_empty()
        {
            return None;
        }
        let visible = fingerprint(&model_visible_parts(&self.parts));
        let bytes = self.parts.iter().map(value_memory).sum::<usize>()
            + self.parts.capacity() * std::mem::size_of::<Value>()
            + self.scope.account.capacity()
            + self.scope.model.capacity()
            + self.scope.session.capacity()
            + self
                .call_ids
                .iter()
                .map(|id| id.capacity() + 64)
                .sum::<usize>()
            + std::mem::size_of::<RecordedTurn>()
            + 128;
        Some(RecordedTurn {
            scope: self.scope,
            context: self.context,
            visible,
            parts: self.parts,
            call_ids: self.call_ids,
            bytes,
        })
    }
}

struct Entry {
    turn: RecordedTurn,
    recorded_at: Instant,
}

pub(crate) struct ReplayCache {
    entries: VecDeque<Entry>,
    bytes: usize,
    max_bytes: usize,
    max_turn_bytes: usize,
    max_turns: usize,
    ttl: Duration,
}

impl Default for ReplayCache {
    fn default() -> Self {
        Self::with_limits(
            8 * 1024 * 1024,
            MAX_TURN_BYTES,
            128,
            Duration::from_secs(3600),
        )
    }
}

impl ReplayCache {
    pub(crate) fn with_limits(
        max_bytes: usize,
        max_turn_bytes: usize,
        max_turns: usize,
        ttl: Duration,
    ) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_bytes,
            max_turn_bytes,
            max_turns,
            ttl,
        }
    }

    pub(crate) fn record(&mut self, turn: RecordedTurn) -> bool {
        self.prune(Instant::now());
        let native_size = serialized_size(&turn.parts);
        if turn.scope.session.trim().is_empty()
            || self.max_turns == 0
            || native_size > self.max_turn_bytes
            || turn.bytes > self.max_bytes
        {
            return false;
        }
        // A retry at the same context replaces its previous signatures rather
        // than accumulating ambiguous alternative entries.
        if let Some(index) = self.entries.iter().position(|entry| {
            entry.turn.scope == turn.scope
                && entry.turn.context == turn.context
                && entry.turn.visible == turn.visible
        }) {
            let old = self.entries.remove(index).unwrap();
            self.bytes -= old.turn.bytes;
        }
        while !self.entries.is_empty()
            && (self.entries.len() >= self.max_turns
                || self.bytes.saturating_add(turn.bytes) > self.max_bytes)
        {
            self.remove_oldest();
        }
        self.bytes += turn.bytes;
        self.entries.push_back(Entry {
            turn,
            recorded_at: Instant::now(),
        });
        true
    }

    /// Validate the complete visible history before changing any parts. Calls
    /// without replayable signatures fail locally, including expired/missing
    /// turns. A stable session is required for all cache access.
    pub(crate) fn restore(&mut self, scope: &Scope, request: &mut Value) -> Result<(), String> {
        self.prune(Instant::now());
        let path = if request.get("contents").is_some() {
            "/contents"
        } else {
            "/request/contents"
        };
        let contents = request
            .pointer(path)
            .and_then(Value::as_array)
            .ok_or("expected native Gemini contents array for signed-history replay")?;
        let mut context = history_hasher();
        let mut replacements = Vec::new();
        for (index, content) in contents.iter().enumerate() {
            if content.get("role").and_then(Value::as_str) == Some("model") {
                let parts = content
                    .get("parts")
                    .and_then(Value::as_array)
                    .ok_or("expected model parts array for signed-history replay")?;
                let current_context: Fingerprint = context.clone().finalize().into();
                let visible = fingerprint(&model_visible_parts(parts));
                let hit = (!scope.session.trim().is_empty())
                    .then(|| {
                        self.entries.iter().rev().find(|entry| {
                            entry.turn.scope == *scope
                                && entry.turn.context == current_context
                                && entry.turn.visible == visible
                        })
                    })
                    .flatten();
                if let Some(entry) = hit {
                    replacements.push((index, entry.turn.parts.clone()));
                } else {
                    let ids = parts.iter().filter_map(|part| {
                        part.get("functionCall")
                            .and_then(|call| call.get("id"))
                            .and_then(Value::as_str)
                    });
                    if ids.into_iter().any(|id| {
                        self.entries
                            .iter()
                            .any(|entry| entry.turn.call_ids.contains(id))
                    }) {
                        return Err("signed Gemini tool history does not match its recorded credential, model, session or preceding context".into());
                    }
                    if parts.iter().any(|part| part.get("functionCall").is_some())
                        && !calls_are_signed(parts)
                    {
                        return Err(if scope.session.trim().is_empty() {
                            "signed Gemini tool history requires a stable session identifier"
                        } else {
                            "signed Gemini tool history is unavailable for this credential, model and session; start a new conversation after cache expiry or restart"
                        }.into());
                    }
                }
            }
            hash_content(&mut context, content);
        }
        let contents = request
            .pointer_mut(path)
            .and_then(Value::as_array_mut)
            .unwrap();
        for (index, parts) in replacements {
            contents[index]["parts"] = Value::Array(parts);
        }
        Ok(())
    }

    fn prune(&mut self, now: Instant) {
        while self
            .entries
            .front()
            .is_some_and(|entry| now.saturating_duration_since(entry.recorded_at) >= self.ttl)
        {
            self.remove_oldest();
        }
    }

    fn remove_oldest(&mut self) {
        if let Some(entry) = self.entries.pop_front() {
            self.bytes -= entry.turn.bytes;
        }
    }
}

fn has_signature(part: &Value) -> bool {
    ["thoughtSignature", "thought_signature"]
        .into_iter()
        .any(|key| {
            part.get(key)
                .and_then(Value::as_str)
                .is_some_and(|signature| !signature.is_empty())
        })
}

fn calls_are_signed(parts: &[Value]) -> bool {
    parts.iter().enumerate().all(|(index, part)| {
        part.get("functionCall").is_none()
            || has_signature(part)
            || index
                .checked_sub(1)
                .is_some_and(|previous| detached_signature(&parts[previous]))
            || parts.get(index + 1).is_some_and(detached_signature)
    })
}

fn detached_signature(part: &Value) -> bool {
    has_signature(part) && part.get("functionCall").is_none() && part.get("text").is_none()
}

fn mergeable_text(left: &Value, right: &Value) -> bool {
    let (Some(left), Some(right)) = (left.as_object(), right.as_object()) else {
        return false;
    };
    if left.get("text").and_then(Value::as_str).is_none()
        || right.get("text").and_then(Value::as_str).is_none()
        || [
            "thoughtSignature",
            "thought_signature",
            "functionCall",
            "functionResponse",
        ]
        .iter()
        .any(|key| left.contains_key(*key) || right.contains_key(*key))
    {
        return false;
    }
    left.len() == right.len()
        && left
            .iter()
            .all(|(key, value)| key == "text" || right.get(key) == Some(value))
}

fn visible_parts(parts: &[Value]) -> Value {
    let mut visible: Vec<Value> = Vec::new();
    for part in parts {
        if part.get("thought").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let mut part = part.clone();
        if let Some(object) = part.as_object_mut() {
            object.remove("thoughtSignature");
            object.remove("thought_signature");
            object.remove("thought");
            if object.is_empty() {
                continue;
            }
        }
        if let Some(last) = visible
            .last_mut()
            .filter(|last| mergeable_text(last, &part))
        {
            if let Value::String(text) = &mut last["text"] {
                text.push_str(part["text"].as_str().unwrap());
            }
        } else {
            visible.push(part);
        }
    }
    Value::Array(visible)
}

/// Chat-compatible clients store one text field and a separate ordered call
/// list. Match that projection while keeping the original native ordering in
/// RecordedTurn. User parts retain their order in the preceding-history hash.
fn model_visible_parts(parts: &[Value]) -> Value {
    let Value::Array(parts) = visible_parts(parts) else {
        unreachable!("visible parts are always an array");
    };
    let mut text = String::new();
    let mut other_parts = Vec::new();
    for mut part in parts {
        if let Some(delta) = part.get("text").and_then(Value::as_str) {
            text.push_str(delta);
            part.as_object_mut().unwrap().remove("text");
            if part.as_object().unwrap().is_empty() {
                continue;
            }
        }
        other_parts.push(part);
    }
    if !text.is_empty() {
        other_parts.insert(0, serde_json::json!({"text":text}));
    }
    Value::Array(other_parts)
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort_unstable();
            let mut sorted = Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonical(&object[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(array) => Value::Array(array.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}

fn history_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(b"douane-gemini-native-history-v1\0");
    hasher
}

fn hash_content(hasher: &mut Sha256, content: &Value) {
    let mut content = content.clone();
    if let Some(parts) = content.get("parts").and_then(Value::as_array) {
        content["parts"] = if content.get("role").and_then(Value::as_str) == Some("model") {
            model_visible_parts(parts)
        } else {
            visible_parts(parts)
        };
    }
    let encoded = serde_json::to_vec(&canonical(&content)).expect("JSON values serialize");
    hasher.update((encoded.len() as u64).to_be_bytes());
    hasher.update(encoded);
}

fn fingerprint(value: &Value) -> Fingerprint {
    Sha256::digest(serde_json::to_vec(&canonical(value)).expect("JSON values serialize")).into()
}

struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_size(value: &impl serde::Serialize) -> usize {
    let mut counter = ByteCounter(0);
    serde_json::to_writer(&mut counter, value).expect("JSON values serialize");
    counter.0
}

/// Conservatively account for heap allocations, including object/tree overhead.
fn value_memory(value: &Value) -> usize {
    std::mem::size_of::<Value>()
        + match value {
            Value::String(text) => text.capacity(),
            Value::Array(array) => {
                array.capacity() * std::mem::size_of::<Value>()
                    + array.iter().map(value_memory).sum::<usize>()
            }
            Value::Object(object) => object
                .iter()
                .map(|(key, value)| key.capacity() + 128 + value_memory(value))
                .sum::<usize>(),
            _ => 0,
        }
}

#[cfg(test)]
mod tests;
