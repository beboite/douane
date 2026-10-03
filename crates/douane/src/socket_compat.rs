//! Responses WebSocket compatibility for subscriptions with HTTP transports.
use std::{collections::VecDeque, sync::Arc, time::Duration};

use axum::{
    body::Bytes,
    extract::{
        OriginalUri, State,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{sync::mpsc, task::JoinHandle};

use crate::{relay, state::AppState, translate::Frames};

const HISTORY_LIMIT: usize = 8;
const HISTORY_BYTES: usize = 64 * 1024 * 1024;
const PENDING_LIMIT: usize = 64;
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

struct History {
    id: String,
    input: Vec<Value>,
    output: Vec<Value>,
    bytes: usize,
}
struct Cache {
    entries: VecDeque<History>,
    bytes: usize,
}
impl Cache {
    fn insert(&mut self, entry: History) {
        if entry.bytes > HISTORY_BYTES {
            return;
        }
        while self.entries.len() >= HISTORY_LIMIT || self.bytes + entry.bytes > HISTORY_BYTES {
            if let Some(old) = self.entries.pop_front() {
                self.bytes -= old.bytes;
            } else {
                break;
            }
        }
        self.bytes += entry.bytes;
        self.entries.push_back(entry);
    }
}

enum Event {
    Frame(Value),
    Finished(Option<History>),
}
struct Active {
    task: JoinHandle<()>,
    events: mpsc::Receiver<Event>,
    lane: Option<Value>,
    response_id: Option<String>,
}
impl Drop for Active {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn input(body: &Value, cache: &Cache) -> Result<Vec<Value>, (&'static str, &'static str)> {
    let mut items = vec![];
    if let Some(previous) = body
        .get("previous_response_id")
        .filter(|value| !value.is_null())
    {
        let id = previous.as_str().ok_or((
            "invalid_request_error",
            "previous_response_id must be a string",
        ))?;
        let previous = cache.entries.iter().find(|entry| entry.id == id).ok_or((
            "previous_response_not_found",
            "previous response is unavailable on this connection; resend the full conversation",
        ))?;
        items.extend(previous.input.iter().cloned());
        items.extend(previous.output.iter().cloned());
    }
    match body.get("input") {
        Some(Value::String(text)) => items.push(
            json!({"type":"message","role":"user","content":[{"type":"input_text","text":text}]}),
        ),
        Some(Value::Array(values)) => items.extend(values.iter().cloned()),
        None | Some(Value::Null) => {}
        _ => {
            return Err((
                "invalid_request_error",
                "input must be a string or an array",
            ));
        }
    }
    if serde_json::to_vec(&items)
        .map_err(|_| ("invalid_request_error", "input cannot be serialized"))?
        .len()
        > HISTORY_BYTES
    {
        return Err(("invalid_request_error", "conversation exceeds 64 MiB"));
    }
    Ok(items)
}

fn failure(code: &str, message: &str) -> Value {
    json!({"type":"error","error":{"type":"invalid_request_error","code":code,"message":message}})
}

async fn emit(sender: &mpsc::Sender<Event>, event: Event) -> bool {
    matches!(
        tokio::time::timeout(WRITE_TIMEOUT, sender.send(event)).await,
        Ok(Ok(()))
    )
}

async fn pump(
    state: Arc<AppState>,
    headers: HeaderMap,
    mut request: Value,
    items: Vec<Value>,
    sender: mpsc::Sender<Event>,
) {
    let object = request
        .as_object_mut()
        .expect("validated response.create object");
    for field in ["type", "generate", "stream_id", "previous_response_id"] {
        object.remove(field);
    }
    object.insert("stream".into(), json!(true));
    object.insert("input".into(), json!(items));
    let body = match serde_json::to_vec(&request) {
        Ok(body) => Bytes::from(body),
        Err(_) => {
            let _ = emit(
                &sender,
                Event::Frame(failure(
                    "invalid_request_error",
                    "request cannot be serialized",
                )),
            )
            .await;
            return;
        }
    };
    let response = relay::forward(
        State(state),
        OriginalUri("/v1/responses".parse().unwrap()),
        headers,
        body,
    )
    .await;
    let status = response.status();
    let mut stream = response.into_body().into_data_stream();
    if status != StatusCode::OK {
        let mut bytes = Vec::new();
        while let Some(Ok(chunk)) = stream.next().await {
            if bytes.len() + chunk.len() > 64 * 1024 {
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Value = serde_json::from_slice(&bytes).unwrap_or_default();
        let message = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("upstream HTTP request failed");
        let _ = emit(&sender, Event::Frame(json!({"type":"error","status":status.as_u16(),"error":{"type":"proxy_error","code":"upstream_error","message":message}}))).await;
        let _ = emit(&sender, Event::Finished(None)).await;
        return;
    }
    let mut decoder = Frames::default();
    let mut history = None;
    let mut total = 0;
    loop {
        let next = stream.next().await;
        let (frames, eof) = match next {
            Some(Ok(chunk)) => {
                total += chunk.len();
                if total > HISTORY_BYTES {
                    let _ = emit(
                        &sender,
                        Event::Frame(failure("response_too_large", "response exceeds 64 MiB")),
                    )
                    .await;
                    break;
                }
                (decoder.feed(&chunk), false)
            }
            Some(Err(_)) => {
                let _ = emit(
                    &sender,
                    Event::Frame(failure(
                        "upstream_error",
                        "upstream response was interrupted",
                    )),
                )
                .await;
                break;
            }
            None => (decoder.finish(), true),
        };
        let frames = match frames {
            Ok(frames) => frames,
            Err(_) => {
                let _ = emit(
                    &sender,
                    Event::Frame(failure(
                        "upstream_error",
                        "upstream returned invalid response events",
                    )),
                )
                .await;
                break;
            }
        };
        for frame in frames {
            if matches!(
                frame["type"].as_str(),
                Some("response.completed" | "response.incomplete")
            ) {
                let response = &frame["response"];
                if let (Some(id), Some(output)) =
                    (response["id"].as_str(), response["output"].as_array())
                {
                    let bytes = serde_json::to_vec(&items)
                        .map_or(HISTORY_BYTES + 1, |value| value.len())
                        + serde_json::to_vec(output).map_or(HISTORY_BYTES + 1, |value| value.len());
                    if bytes <= HISTORY_BYTES {
                        history = Some(History {
                            id: id.into(),
                            input: items.clone(),
                            output: output.clone(),
                            bytes,
                        });
                    }
                }
            }
            if frame["type"] != "__douane_stream_done" && !emit(&sender, Event::Frame(frame)).await
            {
                return;
            }
        }
        if eof {
            break;
        }
    }
    let _ = emit(&sender, Event::Finished(history)).await;
}

pub(crate) async fn run(
    downstream: WebSocket,
    state: Arc<AppState>,
    headers: HeaderMap,
    initial: Value,
) {
    let default_model = initial.get("model").cloned();
    let cache_key = initial
        .get("prompt_cache_key")
        .cloned()
        .or_else(|| crate::affinity::session_key(&headers, &initial).map(Value::String));
    let (mut writer, mut reader) = downstream.split();
    let mut cache = Cache {
        entries: VecDeque::new(),
        bytes: 0,
    };
    let mut pending_bytes =
        serde_json::to_vec(&initial).map_or(HISTORY_BYTES + 1, |value| value.len());
    if pending_bytes > HISTORY_BYTES {
        return;
    }
    let mut pending = VecDeque::from([(initial, pending_bytes)]);
    let mut active: Option<Active> = None;
    let mut last_client = tokio::time::Instant::now();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(25));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.tick().await;
    loop {
        if active.is_none() {
            if let Some((mut request, bytes)) = pending.pop_front() {
                pending_bytes -= bytes;
                if !request.is_object() || request["type"] != "response.create" {
                    break;
                }
                if request.get("model").is_none() {
                    if let Some(model) = &default_model {
                        request["model"] = model.clone();
                    }
                }
                if request.get("prompt_cache_key").is_none() {
                    if let Some(key) = &cache_key {
                        request["prompt_cache_key"] = key.clone();
                    }
                }
                let lane = request.get("stream_id").cloned();
                let items = match input(&request, &cache) {
                    Ok(items) => items,
                    Err((code, message)) => {
                        let mut event = failure(code, message);
                        if let Some(lane) = lane {
                            event["stream_id"] = lane;
                        }
                        if !matches!(
                            tokio::time::timeout(
                                WRITE_TIMEOUT,
                                writer.send(Message::Text(event.to_string().into()))
                            )
                            .await,
                            Ok(Ok(()))
                        ) {
                            break;
                        }
                        continue;
                    }
                };
                if request["generate"] == false {
                    let id = format!("resp_{:032x}", rand::random::<u128>());
                    let response = json!({"id":id,"object":"response","status":"completed","model":request["model"],"output":[],"usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0,"input_tokens_details":{"cached_tokens":0}}});
                    for kind in ["response.created", "response.completed"] {
                        let mut event = json!({"type":kind,"response":response});
                        if let Some(lane) = &lane {
                            event["stream_id"] = lane.clone();
                        }
                        if !matches!(
                            tokio::time::timeout(
                                WRITE_TIMEOUT,
                                writer.send(Message::Text(event.to_string().into()))
                            )
                            .await,
                            Ok(Ok(()))
                        ) {
                            return;
                        }
                    }
                    let bytes =
                        serde_json::to_vec(&items).map_or(HISTORY_BYTES + 1, |value| value.len());
                    cache.insert(History {
                        id,
                        input: items,
                        output: vec![],
                        bytes,
                    });
                    continue;
                }
                let (sender, events) = mpsc::channel(1);
                let task =
                    tokio::spawn(pump(state.clone(), headers.clone(), request, items, sender));
                active = Some(Active {
                    task,
                    events,
                    lane,
                    response_id: None,
                });
            }
        }
        tokio::select! {
            message = reader.next() => {
                last_client = tokio::time::Instant::now();
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let request: Value = match serde_json::from_str(&text) {
                            Ok(value) => value,
                            Err(_) => { let _ = tokio::time::timeout(WRITE_TIMEOUT, writer.send(Message::Text(failure("invalid_request_error", "websocket message must be valid JSON").to_string().into()))).await; break; },
                        };
                        match request["type"].as_str() {
                            Some("response.create") if pending.len() < PENDING_LIMIT && pending_bytes + text.len() <= HISTORY_BYTES => { pending_bytes += text.len(); pending.push_back((request, text.len())); },
                            Some("response.cancel") => {
                                let lane = request.get("stream_id");
                                let id = request.get("response_id").and_then(Value::as_str);
                                let cancel = active.as_ref().is_some_and(|active| lane.is_none_or(|lane| active.lane.as_ref() == Some(lane)) && id.is_none_or(|id| active.response_id.as_deref() == Some(id)));
                                if cancel {
                                    let previous = active.take().unwrap();
                                    let response = json!({"id":previous.response_id,"object":"response","status":"incomplete","output":[],"incomplete_details":{"reason":"cancelled"}});
                                    let mut event = json!({"type":"response.incomplete","response":response});
                                    if let Some(lane) = &previous.lane { event["stream_id"] = lane.clone(); }
                                    drop(previous);
                                    if !matches!(tokio::time::timeout(WRITE_TIMEOUT, writer.send(Message::Text(event.to_string().into()))).await, Ok(Ok(()))) { break; }
                                }
                                if let Some(lane) = lane { pending.retain(|(item, bytes)| { let keep = item.get("stream_id") != Some(lane); if !keep { pending_bytes -= bytes; } keep }); }
                                else if id.is_none() { pending.clear(); pending_bytes = 0; }
                            },
                            _ => { let _ = tokio::time::timeout(WRITE_TIMEOUT, writer.send(Message::Text(failure("invalid_request_error", "unsupported websocket event or pending responses exceed 64 entries or 64 MiB").to_string().into()))).await; break; },
                        }
                    },
                    Some(Ok(Message::Ping(data))) => { if !matches!(tokio::time::timeout(WRITE_TIMEOUT, writer.send(Message::Pong(data))).await, Ok(Ok(()))) { break; } },
                    Some(Ok(Message::Pong(_))) => {},
                    _ => break,
                }
            },
            event = async { active.as_mut().unwrap().events.recv().await }, if active.is_some() => {
                match event {
                    Some(Event::Frame(mut frame)) => {
                        let request = active.as_mut().unwrap();
                        if let Some(id) = frame.pointer("/response/id").and_then(Value::as_str) { request.response_id = Some(id.into()); }
                        if let Some(lane) = &request.lane { frame["stream_id"] = lane.clone(); }
                        if !matches!(tokio::time::timeout(WRITE_TIMEOUT, writer.send(Message::Text(frame.to_string().into()))).await, Ok(Ok(()))) { break; }
                    },
                    Some(Event::Finished(history)) => { if let Some(history) = history { cache.insert(history); } active = None; },
                    None => { active = None; },
                }
            },
            _ = heartbeat.tick() => {
                if last_client.elapsed() > Duration::from_secs(90) { break; }
                if !matches!(tokio::time::timeout(WRITE_TIMEOUT, writer.send(Message::Ping(Bytes::new()))).await, Ok(Ok(()))) { break; }
            },
        }
    }
    drop(active);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer.close()).await;
}
