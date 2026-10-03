//! Translation at protocol boundaries. Native requests and streams bypass this module.

use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Messages,
    Chat,
    Responses,
    Gemini,
}

pub fn request(body: &Value, from: Dialect, to: Dialect) -> Result<Value, String> {
    if from == to {
        return Ok(body.clone());
    }
    let chat = match from {
        Dialect::Chat => body.clone(),
        Dialect::Messages => serde_json::from_slice(
            &crate::wire::request_to_openai(&serde_json::to_vec(body).map_err(|_| "invalid JSON")?)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|_| "invalid translated request")?,
        Dialect::Responses => responses_to_chat(body)?,
        Dialect::Gemini => {
            return Err("native Gemini input is not supported on this endpoint".into());
        }
    };
    match to {
        Dialect::Chat => Ok(chat),
        Dialect::Messages => chat_to_messages(&chat),
        Dialect::Responses => chat_to_responses(&chat),
        Dialect::Gemini => chat_to_gemini(&chat),
    }
}

fn copy_fields(from: &Value, to: &mut Value, keys: &[&str]) {
    for key in keys {
        if let Some(value) = from.get(*key) {
            to[*key] = value.clone();
        }
    }
}
fn text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.into();
    }
    content
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn responses_to_chat(body: &Value) -> Result<Value, String> {
    let mut messages = vec![];
    if let Some(instructions) = body.get("instructions").filter(|v| !v.is_null()) {
        messages.push(json!({"role":"system","content":instructions}));
    }
    match body.get("input") {
        Some(Value::String(s)) => messages.push(json!({"role":"user","content":s})),
        Some(Value::Array(items)) => {
            for item in items {
                match item["type"].as_str().unwrap_or("message") {
                "message" => {
                    let mut content=item["content"].clone();
                    if let Some(blocks)=content.as_array_mut(){ for block in blocks {
                        match block["type"].as_str().unwrap_or("") {
                            "input_text"|"output_text" => block["type"]=json!("text"),
                            "input_image" => *block=json!({"type":"image_url","image_url":{"url":block["image_url"],"detail":block.get("detail").cloned().unwrap_or(json!("auto"))}}),
                            other => return Err(format!("cannot translate Responses content type {other}")),
                        }
                    }}
                    messages.push(json!({"role":item["role"],"content":content}));
                },
                "function_call" => messages.push(json!({"role":"assistant","content":null,"tool_calls":[{"id":item["call_id"],"type":"function","function":{"name":item["name"],"arguments":item["arguments"]}}]})),
                "function_call_output" => messages.push(json!({"role":"tool","tool_call_id":item["call_id"],"content":item["output"]})),
                "reasoning" => {},
                other => return Err(format!("cannot translate Responses item type {other}")),
            }
            }
        }
        _ => return Err("input must be a string or array".into()),
    }
    let mut out = json!({"model":body["model"],"messages":messages});
    copy_fields(
        body,
        &mut out,
        &[
            "stream",
            "temperature",
            "top_p",
            "parallel_tool_calls",
            "service_tier",
            "prompt_cache_key",
            "prompt_cache_retention",
        ],
    );
    if let Some(n) = body.get("max_output_tokens") {
        out["max_tokens"] = n.clone();
    }
    if body.get("stream").and_then(Value::as_bool) == Some(true) {
        out["stream_options"] = json!({"include_usage":true});
    }
    if let Some(e) = body.pointer("/reasoning/effort") {
        out["reasoning_effort"] = e.clone();
    }
    if let Some(tools) = body["tools"].as_array() {
        let mut mapped = vec![];
        for tool in tools {
            if tool["type"] != "function" {
                return Err("this provider cannot translate non-function Responses tools".into());
            }
            let mut f = tool.clone();
            f.as_object_mut().unwrap().remove("type");
            mapped.push(json!({"type":"function","function":f}));
        }
        out["tools"] = json!(mapped);
    }
    if let Some(choice) = body.get("tool_choice") {
        out["tool_choice"] = if choice["type"] == "function" {
            json!({"type":"function","function":{"name":choice["name"]}})
        } else {
            choice.clone()
        };
    }
    Ok(out)
}

fn chat_to_responses(body: &Value) -> Result<Value, String> {
    let mut input = vec![];
    let mut instructions = vec![];
    for m in body["messages"]
        .as_array()
        .ok_or("messages must be an array")?
    {
        match m["role"].as_str().unwrap_or("user") {
            "system"|"developer"=>instructions.push(text(&m["content"])),
            "tool"=>input.push(json!({"type":"function_call_output","call_id":m["tool_call_id"],"output":if m["content"].is_string(){m["content"].clone()}else{json!(text(&m["content"]))}})),
            role=>{
                if !m["content"].is_null(){
                    let mut content=vec![];
                    if let Some(s)=m["content"].as_str(){content.push(json!({"type":if role=="assistant"{"output_text"}else{"input_text"},"text":s}));}
                    else if let Some(parts)=m["content"].as_array(){for p in parts {
                        match p["type"].as_str().unwrap_or("text"){
                            "text"=>content.push(json!({"type":if role=="assistant"{"output_text"}else{"input_text"},"text":p["text"]})),
                            "image_url"=>content.push(json!({"type":"input_image","image_url":p.pointer("/image_url/url").unwrap_or(&p["image_url"]),"detail":p.pointer("/image_url/detail").cloned().unwrap_or(json!("auto"))})),
                            _=>return Err("unsupported chat content for Responses".into()),
                        }
                    }}
                    if !content.is_empty(){input.push(json!({"type":"message","role":role,"content":content}));}
                }
                if let Some(calls)=m["tool_calls"].as_array(){for call in calls { input.push(json!({"type":"function_call","call_id":call["id"],"name":call["function"]["name"],"arguments":call["function"]["arguments"]})); }}
            }
        }
    }
    let mut out = json!({"model":body["model"],"input":input,"instructions":instructions.join("\n\n"),"store":false});
    copy_fields(
        body,
        &mut out,
        &[
            "stream",
            "temperature",
            "top_p",
            "parallel_tool_calls",
            "service_tier",
            "prompt_cache_key",
            "prompt_cache_retention",
        ],
    );
    if let Some(n) = body
        .get("max_completion_tokens")
        .or_else(|| body.get("max_tokens"))
    {
        out["max_output_tokens"] = n.clone();
    }
    if let Some(e) = body.get("reasoning_effort") {
        out["reasoning"] = json!({"effort":e});
    }
    if let Some(tools) = body["tools"].as_array() {
        out["tools"] = json!(
            tools
                .iter()
                .map(|t| {
                    let mut f = t["function"].clone();
                    f["type"] = json!("function");
                    f
                })
                .collect::<Vec<_>>()
        );
    }
    if let Some(choice) = body.get("tool_choice") {
        out["tool_choice"] = if choice["type"] == "function" {
            json!({"type":"function","name":choice["function"]["name"]})
        } else {
            choice.clone()
        };
    }
    Ok(out)
}

fn chat_to_messages(body: &Value) -> Result<Value, String> {
    let mut system = vec![];
    let mut messages: Vec<Value> = vec![];
    for m in body["messages"]
        .as_array()
        .ok_or("messages must be an array")?
    {
        let role = m["role"].as_str().unwrap_or("user");
        if matches!(role, "system" | "developer") {
            system.push(text(&m["content"]));
            continue;
        }
        let mut content = vec![];
        if role == "tool" {
            content.push(json!({"type":"tool_result","tool_use_id":m["tool_call_id"],"content":text(&m["content"])}));
        } else {
            if let Some(s) = m["content"].as_str() {
                if !s.is_empty() {
                    content.push(json!({"type":"text","text":s}));
                }
            }
            if let Some(parts) = m["content"].as_array() {
                for p in parts {
                    match p["type"].as_str().unwrap_or("text") {
                        "text" => content.push(p.clone()),
                        "image_url" => {
                            let url = p
                                .pointer("/image_url/url")
                                .and_then(Value::as_str)
                                .ok_or("image_url.url is required")?;
                            if let Some(data) = url.strip_prefix("data:") {
                                let (media, data) =
                                    data.split_once(";base64,").ok_or("invalid base64 image")?;
                                content.push(json!({"type":"image","source":{"type":"base64","media_type":media,"data":data}}));
                            } else {
                                content.push(
                                    json!({"type":"image","source":{"type":"url","url":url}}),
                                );
                            }
                        }
                        _ => return Err("unsupported chat content for Messages".into()),
                    }
                }
            }
            if let Some(calls) = m["tool_calls"].as_array() {
                for call in calls {
                    let input = serde_json::from_str::<Value>(
                        call["function"]["arguments"].as_str().unwrap_or("{}"),
                    )
                    .map_err(|_| "tool arguments are not JSON")?;
                    content.push(json!({"type":"tool_use","id":call["id"],"name":call["function"]["name"],"input":input}));
                }
            }
        }
        let target_role = if role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        if !content.is_empty() {
            if let Some(previous) = messages.last_mut().filter(|p| p["role"] == target_role) {
                previous["content"].as_array_mut().unwrap().extend(content);
            } else {
                messages.push(json!({"role":target_role,"content":content}));
            }
        }
    }
    let mut out = json!({"model":body["model"],"messages":messages,"max_tokens":body.get("max_tokens").or_else(||body.get("max_completion_tokens")).cloned().unwrap_or(json!(8192))});
    if !system.is_empty() {
        out["system"] = json!(system.join("\n\n"));
    }
    copy_fields(body, &mut out, &["stream", "temperature", "top_p"]);
    if let Some(stop) = body.get("stop") {
        out["stop_sequences"] = if stop.is_array() {
            stop.clone()
        } else {
            json!([stop])
        };
    }
    if let Some(tools) = body["tools"].as_array() {
        out["tools"]=json!(tools.iter().map(|t|json!({"name":t["function"]["name"],"description":t["function"].get("description").cloned().unwrap_or(json!("")),"input_schema":t["function"].get("parameters").cloned().unwrap_or(json!({"type":"object","properties":{}}))})).collect::<Vec<_>>());
    }
    if let Some(choice) = body.get("tool_choice") {
        out["tool_choice"] = match choice.as_str() {
            Some("required") => json!({"type":"any"}),
            Some("none") => json!({"type":"none"}),
            Some("auto") => json!({"type":"auto"}),
            _ => json!({"type":"tool","name":choice["function"]["name"]}),
        };
    }
    Ok(out)
}

fn chat_to_gemini(body: &Value) -> Result<Value, String> {
    let mut contents = vec![];
    let mut system = vec![];
    let mut call_names = BTreeMap::new();
    for m in body["messages"]
        .as_array()
        .ok_or("messages must be an array")?
    {
        let role = m["role"].as_str().unwrap_or("user");
        if matches!(role, "system" | "developer") {
            system.push(json!({"text":text(&m["content"])}));
            continue;
        }
        let mut parts = vec![];
        if role == "tool" {
            let id = m["tool_call_id"].as_str().unwrap_or("");
            let name = call_names
                .get(id)
                .cloned()
                .ok_or("tool result refers to an unknown function call")?;
            parts
                .push(json!({"functionResponse":{"name":name,"response":{"result":m["content"]}}}));
        } else {
            if let Some(t) = m["content"].as_str() {
                if !t.is_empty() {
                    parts.push(json!({"text":t}));
                }
            } else if let Some(blocks) = m["content"].as_array() {
                for block in blocks {
                    match block["type"].as_str() {
                        Some("text") => parts.push(json!({"text":block["text"]})),
                        Some("image_url") => {
                            let url = block
                                .pointer("/image_url/url")
                                .and_then(Value::as_str)
                                .or_else(|| block["image_url"].as_str())
                                .ok_or("image URL is missing")?;
                            if let Some(data) = url.strip_prefix("data:") {
                                let (media, data) = data
                                    .split_once(";base64,")
                                    .ok_or("image data URL is not base64")?;
                                parts.push(json!({"inlineData":{"mimeType":media,"data":data}}));
                            } else {
                                return Err(
                                    "Gemini image translation requires a base64 data URL".into()
                                );
                            }
                        }
                        _ => return Err("unsupported content in Gemini request translation".into()),
                    }
                }
            } else if !m["content"].is_null() {
                return Err("message content must be text or an array".into());
            }
            if let Some(calls) = m["tool_calls"].as_array() {
                for call in calls {
                    let name = call["function"]["name"].as_str().unwrap_or("");
                    call_names.insert(
                        call["id"].as_str().unwrap_or("").to_owned(),
                        name.to_owned(),
                    );
                    parts.push(json!({"functionCall":{"name":name,"args":serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap_or("{}")).map_err(|_|"invalid tool arguments")?}}));
                }
            }
        }
        if !parts.is_empty() {
            contents.push(json!({"role":if role=="assistant"{"model"}else{"user"},"parts":parts}));
        }
    }
    let mut out = json!({"contents":contents});
    if !system.is_empty() {
        out["systemInstruction"] = json!({"role":"user","parts":system});
    }
    let mut generation = json!({});
    for (source, target) in [
        ("temperature", "temperature"),
        ("top_p", "topP"),
        ("max_tokens", "maxOutputTokens"),
        ("max_completion_tokens", "maxOutputTokens"),
    ] {
        if let Some(v) = body.get(source) {
            generation[target] = v.clone();
        }
    }
    out["generationConfig"] = generation;
    if let Some(tools) = body["tools"].as_array() {
        out["tools"] = json!([{"functionDeclarations":tools.iter().map(|t|t["function"].clone()).collect::<Vec<_>>()}]);
    }
    if let Some(choice) = body.get("tool_choice") {
        let config = match choice.as_str() {
            Some("auto") => json!({"mode":"AUTO"}),
            Some("none") => json!({"mode":"NONE"}),
            Some("required") => json!({"mode":"ANY"}),
            _ if choice["type"] == "function" => {
                json!({"mode":"ANY","allowedFunctionNames":[choice["function"]["name"]]})
            }
            _ => return Err("unsupported Gemini tool choice".into()),
        };
        out["toolConfig"] = json!({"functionCallingConfig":config});
    }
    Ok(out)
}

pub fn response(body: &Value, from: Dialect, to: Dialect, model: &str) -> Result<Value, String> {
    if from == to {
        return Ok(body.clone());
    }
    let chat = to_chat_response(body, from, model)?;
    match to {
        Dialect::Chat => Ok(chat),
        Dialect::Messages => serde_json::from_slice(
            &crate::wire::response_to_anthropic(&serde_json::to_vec(&chat).unwrap(), model)
                .ok_or("cannot translate completion")?,
        )
        .map_err(|_| "invalid translated completion".into()),
        Dialect::Responses => Ok(chat_response_to_responses(&chat, model)),
        Dialect::Gemini => Err("Gemini output requires its native endpoint".into()),
    }
}

fn to_chat_response(body: &Value, from: Dialect, model: &str) -> Result<Value, String> {
    if from == Dialect::Chat {
        return Ok(body.clone());
    }
    let mut content = String::new();
    let mut calls = vec![];
    let mut input = 0;
    let mut output = 0;
    let mut cached = 0;
    let mut cache_write = 0;
    let mut finish = "stop";
    match from {
        Dialect::Responses => {
            for item in body["output"]
                .as_array()
                .ok_or("Responses output missing")?
            {
                if item["type"] == "message" {
                    content.push_str(&text(&item["content"]));
                }
                if item["type"] == "function_call" {
                    calls.push(json!({"id":item["call_id"],"type":"function","function":{"name":item["name"],"arguments":item["arguments"]}}));
                }
            }
            input = body["usage"]["input_tokens"].as_u64().unwrap_or(0);
            output = body["usage"]["output_tokens"].as_u64().unwrap_or(0);
            cached = body
                .pointer("/usage/input_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            cache_write = body
                .pointer("/usage/input_tokens_details/cache_write_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if body["status"] == "incomplete" {
                finish = "length";
            }
        }
        Dialect::Messages => {
            for part in body["content"]
                .as_array()
                .ok_or("Messages content missing")?
            {
                if part["type"] == "text" {
                    content.push_str(part["text"].as_str().unwrap_or(""));
                }
                if part["type"] == "tool_use" {
                    calls.push(json!({"id":part["id"],"type":"function","function":{"name":part["name"],"arguments":part["input"].to_string()}}));
                }
            }
            input = body["usage"]["input_tokens"].as_u64().unwrap_or(0);
            output = body["usage"]["output_tokens"].as_u64().unwrap_or(0);
            cached = body["usage"]["cache_read_input_tokens"]
                .as_u64()
                .unwrap_or(0);
            cache_write = body["usage"]["cache_creation_input_tokens"]
                .as_u64()
                .unwrap_or(0);
            input += cached + cache_write;
            if body["stop_reason"] == "max_tokens" {
                finish = "length";
            }
        }
        Dialect::Gemini => {
            let b = body.get("response").unwrap_or(body);
            if let Some(parts) = b
                .pointer("/candidates/0/content/parts")
                .and_then(Value::as_array)
            {
                for p in parts {
                    content.push_str(p["text"].as_str().unwrap_or(""));
                    if let Some(f) = p.get("functionCall") {
                        calls.push(json!({"id":format!("call_{}",calls.len()),"type":"function","function":{"name":f["name"],"arguments":f["args"].to_string()}}));
                    }
                }
            }
            input = b["usageMetadata"]["promptTokenCount"].as_u64().unwrap_or(0);
            cached = b["usageMetadata"]["cachedContentTokenCount"]
                .as_u64()
                .unwrap_or(0);
            output = b["usageMetadata"]["candidatesTokenCount"]
                .as_u64()
                .unwrap_or(0);
            if b["candidates"][0]["finishReason"] == "MAX_TOKENS" {
                finish = "length";
            }
        }
        Dialect::Chat => {}
    }
    if !calls.is_empty() {
        finish = "tool_calls";
    }
    let mut message = json!({"role":"assistant","content":content});
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }
    Ok(
        json!({"id":body.get("id").cloned().unwrap_or(json!("chatcmpl_douane")),"object":"chat.completion","created":time::OffsetDateTime::now_utc().unix_timestamp(),"model":model,"choices":[{"index":0,"message":message,"finish_reason":finish}],"usage":{"prompt_tokens":input,"completion_tokens":output,"total_tokens":input+output,"prompt_tokens_details":{"cached_tokens":cached,"cache_write_tokens":cache_write}}}),
    )
}

fn chat_response_to_responses(body: &Value, model: &str) -> Value {
    let message = &body["choices"][0]["message"];
    let mut output = vec![];
    if let Some(t) = message["content"].as_str().filter(|s| !s.is_empty()) {
        output.push(json!({"type":"message","id":"msg_douane","role":"assistant","status":"completed","content":[{"type":"output_text","text":t,"annotations":[]}]}));
    }
    if let Some(calls) = message["tool_calls"].as_array() {
        for call in calls {
            output.push(json!({"type":"function_call","id":call["id"],"call_id":call["id"],"name":call["function"]["name"],"arguments":call["function"]["arguments"],"status":"completed"}));
        }
    }
    let input = body["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
    let completion = body["usage"]["completion_tokens"].as_u64().unwrap_or(0);
    json!({"id":body["id"],"object":"response","created_at":body["created"],"status":if body["choices"][0]["finish_reason"]=="length"{"incomplete"}else{"completed"},"model":model,"output":output,"error":null,"usage":{"input_tokens":input,"output_tokens":completion,"total_tokens":input+completion,"input_tokens_details":{"cached_tokens":body.pointer("/usage/prompt_tokens_details/cached_tokens").and_then(Value::as_u64).unwrap_or(0),"cache_write_tokens":body.pointer("/usage/prompt_tokens_details/cache_write_tokens").and_then(Value::as_u64).unwrap_or(0)}}})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(output: &str) -> Vec<Value> {
        let mut frames = Frames::default();
        frames.feed(output.as_bytes()).unwrap()
    }

    #[test]
    fn native_passthrough_preserves_cache_controls_and_signatures() {
        let body = json!({"model":"fixture", "system":[{"type":"text","text":"context","cache_control":{"type":"ephemeral"}}],
            "messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"context","signature":"fixture-signature"}]}],
            "prompt_cache_key":"session-fixture","prompt_cache_retention":"24h"});
        for dialect in [Dialect::Messages, Dialect::Chat, Dialect::Responses] {
            assert_eq!(request(&body, dialect, dialect).unwrap(), body);
        }
        let chat = json!({"model":"fixture","messages":[{"role":"user","content":"hello"}],"prompt_cache_key":"stable","prompt_cache_retention":"24h"});
        let translated = request(&chat, Dialect::Chat, Dialect::Responses).unwrap();
        assert_eq!(translated["prompt_cache_key"], "stable");
        assert_eq!(translated["prompt_cache_retention"], "24h");
    }

    #[test]
    fn tools_roundtrip_through_all_three_client_dialects() {
        let chat = json!({"model":"fixture","messages":[
            {"role":"user","content":"read the file"},
            {"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"read","arguments":"{\"path\":\"a.rs\"}"}}]},
            {"role":"tool","tool_call_id":"call_1","content":"content"}],
            "tools":[{"type":"function","function":{"name":"read","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}}],
            "tool_choice":{"type":"function","function":{"name":"read"}}});
        let messages = request(&chat, Dialect::Chat, Dialect::Messages).unwrap();
        let responses = request(&messages, Dialect::Messages, Dialect::Responses).unwrap();
        let roundtrip = request(&responses, Dialect::Responses, Dialect::Chat).unwrap();
        let calls = roundtrip["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|message| message.get("tool_calls"))
            .unwrap();
        assert_eq!(calls[0]["id"], "call_1");
        assert_eq!(calls[0]["function"]["name"], "read");
        assert_eq!(calls[0]["function"]["arguments"], "{\"path\":\"a.rs\"}");
        assert_eq!(
            roundtrip["tools"][0]["function"]["parameters"]["properties"]["path"]["type"],
            "string"
        );
        assert_eq!(roundtrip["tool_choice"]["function"]["name"], "read");
    }

    #[test]
    fn images_reach_responses_messages_and_gemini_without_silent_loss() {
        let messages = json!({"model":"fixture","messages":[{"role":"user","content":[{"type":"text","text":"describe"},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AA=="}}]}]});
        let responses = request(&messages, Dialect::Messages, Dialect::Responses).unwrap();
        assert_eq!(
            responses["input"][0]["content"][1]["image_url"],
            "data:image/png;base64,AA=="
        );
        let gemini = request(&messages, Dialect::Messages, Dialect::Gemini).unwrap();
        assert_eq!(
            gemini["contents"][0]["parts"][1]["inlineData"]["data"],
            "AA=="
        );
        let invalid = json!({"model":"fixture","messages":[{"role":"user","content":[{"type":"input_audio","input_audio":{"data":"AA=="}}]}]});
        assert!(request(&invalid, Dialect::Chat, Dialect::Gemini).is_err());
    }

    #[test]
    fn sse_frames_accept_unicode_split_bytes_mixed_endings_and_final_unterminated_record() {
        let wire = "data: {\"text\":\"été 🦀\"}\r\n\r\ndata: {\"text\":\"next\"}\n\ndata: [DONE]";
        let mut frames = Frames::default();
        let mut decoded = Vec::new();
        for byte in wire.as_bytes() {
            decoded.extend(frames.feed(&[*byte]).unwrap());
        }
        decoded.extend(frames.finish().unwrap());
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0]["text"], "été 🦀");
        assert_eq!(decoded[1]["text"], "next");
        assert_eq!(decoded[2]["type"], "__douane_stream_done");
    }

    #[test]
    fn usage_after_chat_finish_reaches_messages_and_responses() {
        for target in [Dialect::Messages, Dialect::Responses] {
            let mut bridge = Bridge::new(Dialect::Chat, target, "fixture");
            let mut output = bridge
                .feed(json!({"choices":[{"delta":{"content":"ok"},"finish_reason":null}]}))
                .unwrap();
            output.push_str(
                &bridge
                    .feed(json!({"choices":[{"delta":{},"finish_reason":"stop"}]}))
                    .unwrap(),
            );
            assert!(!bridge.finished());
            output.push_str(&bridge.feed(json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":9,"prompt_tokens_details":{"cached_tokens":70,"cache_write_tokens":10}}})).unwrap());
            output.push_str(&bridge.feed(json!({"type":"__douane_stream_done"})).unwrap());
            assert!(bridge.finished());
            let frames = values(&output);
            if target == Dialect::Messages {
                let usage = &frames
                    .iter()
                    .find(|frame| frame["type"] == "message_delta")
                    .unwrap()["usage"];
                assert_eq!(usage["input_tokens"], 20);
                assert_eq!(usage["cache_read_input_tokens"], 70);
                assert_eq!(usage["cache_creation_input_tokens"], 10);
                assert_eq!(usage["output_tokens"], 9);
            } else {
                let usage = &frames
                    .iter()
                    .find(|frame| frame["type"] == "response.completed")
                    .unwrap()["response"]["usage"];
                assert_eq!(usage["input_tokens"], 100);
                assert_eq!(usage["input_tokens_details"]["cached_tokens"], 70);
                assert_eq!(usage["output_tokens"], 9);
            }
        }
    }

    #[test]
    fn streamed_tool_fragments_survive_split_names_and_interleaved_text() {
        for target in [Dialect::Messages, Dialect::Responses] {
            let mut bridge = Bridge::new(Dialect::Chat, target, "fixture");
            let mut output = bridge.feed(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"re","arguments":"{\"path\":"}}]},"finish_reason":null}]})).unwrap();
            output.push_str(
                &bridge
                    .feed(
                        json!({"choices":[{"delta":{"content":"checking"},"finish_reason":null}]}),
                    )
                    .unwrap(),
            );
            output.push_str(&bridge.feed(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"ad","arguments":"\"a.rs\"}"}}]},"finish_reason":null}]})).unwrap());
            output.push_str(
                &bridge
                    .feed(json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}))
                    .unwrap(),
            );
            output.push_str(&bridge.finish().unwrap());
            let frames = values(&output);
            if target == Dialect::Messages {
                let tool = frames
                    .iter()
                    .find(|frame| frame["content_block"]["type"] == "tool_use")
                    .unwrap();
                assert_eq!(tool["content_block"]["name"], "read");
                let args = frames
                    .iter()
                    .find(|frame| frame["delta"]["type"] == "input_json_delta")
                    .unwrap();
                assert_eq!(args["delta"]["partial_json"], "{\"path\":\"a.rs\"}");
            } else {
                let completed = frames
                    .iter()
                    .find(|frame| frame["type"] == "response.completed")
                    .unwrap();
                let tool = completed["response"]["output"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|item| item["type"] == "function_call")
                    .unwrap();
                assert_eq!(tool["name"], "read");
                assert_eq!(tool["arguments"], "{\"path\":\"a.rs\"}");
            }
        }
    }

    #[test]
    fn premature_eof_and_upstream_error_do_not_look_like_finished_answers() {
        let mut bridge = Bridge::new(Dialect::Chat, Dialect::Responses, "fixture");
        bridge
            .feed(json!({"choices":[{"delta":{"content":"partial"},"finish_reason":null}]}))
            .unwrap();
        assert!(bridge.finish().is_err());
        assert!(
            bridge
                .feed(json!({"error":{"message":"fixture-private-upstream"}}))
                .unwrap_err()
                .contains("upstream reported")
        );
    }
}

/// Decodes SSE records without assuming transport chunks end on UTF-8 or frame boundaries.
#[derive(Default)]
pub struct Frames {
    buffer: Vec<u8>,
}
impl Frames {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Value>, String> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > 8 * 1024 * 1024 {
            return Err("upstream SSE frame exceeds 8 MiB".into());
        }
        let mut values = vec![];
        while let Some((end, separator)) = [
            self.buffer
                .windows(2)
                .position(|p| p == b"\n\n")
                .map(|p| (p, 2)),
            self.buffer
                .windows(4)
                .position(|p| p == b"\r\n\r\n")
                .map(|p| (p, 4)),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(end, _)| *end)
        {
            let frame: Vec<u8> = self.buffer.drain(..end + separator).collect();
            let text = std::str::from_utf8(&frame).map_err(|_| "invalid UTF-8 in upstream SSE")?;
            let data = text
                .lines()
                .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
                .collect::<Vec<_>>()
                .join("\n");
            if data == "[DONE]" {
                values.push(json!({"type":"__douane_stream_done"}));
            } else if !data.is_empty() {
                values.push(serde_json::from_str(&data).map_err(|_| "invalid upstream SSE JSON")?);
            }
        }
        Ok(values)
    }

    pub fn finish(&mut self) -> Result<Vec<Value>, String> {
        if self.buffer.iter().all(u8::is_ascii_whitespace) {
            self.buffer.clear();
            return Ok(Vec::new());
        }
        self.feed(b"\n\n")
    }
}

fn sse(event: Option<&str>, data: &Value) -> String {
    match event {
        Some(e) => format!("event: {e}\ndata: {data}\n\n"),
        None => format!("data: {data}\n\n"),
    }
}

pub struct Bridge {
    from: Dialect,
    to: Dialect,
    model: String,
    id: String,
    seq: u64,
    started: bool,
    finished: bool,
    pending_finish: Option<String>,
    source_tools: usize,
    cached: u64,
    cache_write: u64,
    text: String,
    tools: BTreeMap<usize, Value>,
    input: u64,
    output: u64,
    anthropic: crate::wire::StreamTranslator,
}
impl Bridge {
    pub fn new(from: Dialect, to: Dialect, model: &str) -> Self {
        Self {
            from,
            to,
            model: model.into(),
            id: format!("resp_{:016x}", rand::random::<u64>()),
            seq: 0,
            started: false,
            finished: false,
            pending_finish: None,
            source_tools: 0,
            cached: 0,
            cache_write: 0,
            text: String::new(),
            tools: BTreeMap::new(),
            input: 0,
            output: 0,
            anthropic: crate::wire::StreamTranslator::new(model),
        }
    }
    pub fn finished(&self) -> bool {
        self.finished
    }
    pub fn feed(&mut self, value: Value) -> Result<String, String> {
        if self.finished {
            return Ok(String::new());
        }
        if value["type"] == "__douane_stream_done" {
            return self.finish();
        }
        if value["type"] == "error"
            || value["type"] == "response.failed"
            || value.get("error").is_some_and(|v| !v.is_null())
        {
            return Err("upstream reported an error in its stream".into());
        }
        let terminal = matches!(
            value["type"].as_str(),
            Some("response.completed" | "response.incomplete" | "message_stop")
        );
        let chunks = self.into_chat(value)?;
        let mut out = String::new();
        for mut chunk in chunks {
            if let Some(n) = chunk["usage"]["prompt_tokens"].as_u64() {
                self.input = n;
            }
            if let Some(n) = chunk["usage"]["completion_tokens"].as_u64() {
                self.output = n;
            }
            if let Some(n) = chunk
                .pointer("/usage/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
            {
                self.cached = n;
            }
            if let Some(n) = chunk
                .pointer("/usage/prompt_tokens_details/cache_write_tokens")
                .and_then(Value::as_u64)
            {
                self.cache_write = n;
            }
            if let Some(reason) = chunk["choices"][0]["finish_reason"].as_str() {
                self.pending_finish = Some(reason.into());
                if self.to != Dialect::Chat {
                    chunk["choices"][0]["finish_reason"] = Value::Null;
                }
            }
            match self.to {
                Dialect::Chat => out.push_str(&sse(None, &chunk)),
                Dialect::Messages => out.push_str(&String::from_utf8_lossy(
                    &self.anthropic.push(sse(None, &chunk).as_bytes()),
                )),
                Dialect::Responses => out.push_str(&self.responses_chunk(&chunk)?),
                Dialect::Gemini => return Err("unsupported translated Gemini stream".into()),
            }
        }
        if terminal {
            out.push_str(&self.finish()?);
        }
        Ok(out)
    }

    /// Close only after receiving a real finish signal. This lets an OpenAI
    /// usage-only tail arrive after finish_reason without being discarded.
    pub fn finish(&mut self) -> Result<String, String> {
        if self.finished {
            return Ok(String::new());
        }
        let reason = self
            .pending_finish
            .clone()
            .ok_or("upstream stream ended before a completion event")?;
        self.finished = true;
        let final_chunk = self.chunk(json!({}), Some(&reason));
        match self.to {
            Dialect::Chat => Ok("data: [DONE]\n\n".into()),
            Dialect::Messages => {
                let mut output = String::from_utf8_lossy(
                    &self.anthropic.push(sse(None, &final_chunk).as_bytes()),
                )
                .into_owned();
                output.push_str(&String::from_utf8_lossy(&self.anthropic.finish()));
                Ok(output)
            }
            Dialect::Responses => self.responses_chunk(&final_chunk),
            Dialect::Gemini => Err("unsupported translated Gemini stream".into()),
        }
    }
    fn chunk(&self, delta: Value, finish: Option<&str>) -> Value {
        json!({"id":self.id,"object":"chat.completion.chunk","created":time::OffsetDateTime::now_utc().unix_timestamp(),"model":self.model,"choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":{"prompt_tokens":self.input,"completion_tokens":self.output,"total_tokens":self.input+self.output,"prompt_tokens_details":{"cached_tokens":self.cached,"cache_write_tokens":self.cache_write}}})
    }
    fn into_chat(&mut self, v: Value) -> Result<Vec<Value>, String> {
        let mut chunks = vec![];
        match self.from{
            Dialect::Chat=>chunks.push(v),
            Dialect::Responses=>match v["type"].as_str().unwrap_or(""){
                "response.created"=>chunks.push(self.chunk(json!({"role":"assistant"}),None)),
                "response.output_text.delta"=>chunks.push(self.chunk(json!({"content":v["delta"]}),None)),
                "response.output_item.added" if v["item"]["type"]=="function_call"=>chunks.push(self.chunk(json!({"tool_calls":[{"index":v["output_index"],"id":v["item"]["call_id"],"type":"function","function":{"name":v["item"]["name"],"arguments":""}}]}),None)),
                "response.function_call_arguments.delta"=>chunks.push(self.chunk(json!({"tool_calls":[{"index":v["output_index"],"function":{"arguments":v["delta"]}}]}),None)),
                "response.completed"|"response.incomplete"=>{
                    self.input=v["response"]["usage"]["input_tokens"].as_u64().unwrap_or(0);self.output=v["response"]["usage"]["output_tokens"].as_u64().unwrap_or(0);
                    self.cached=v.pointer("/response/usage/input_tokens_details/cached_tokens").and_then(Value::as_u64).unwrap_or(0);
                    self.cache_write=v.pointer("/response/usage/input_tokens_details/cache_write_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let tools=v["response"]["output"].as_array().is_some_and(|a|a.iter().any(|i|i["type"]=="function_call"));
                    chunks.push(self.chunk(json!({}),Some(if tools{"tool_calls"}else if v["type"]=="response.incomplete"{"length"}else{"stop"})));
                },_=>{},
            },
            Dialect::Messages=>match v["type"].as_str().unwrap_or(""){
                "message_start"=>{self.cached=v["message"]["usage"]["cache_read_input_tokens"].as_u64().unwrap_or(0);self.cache_write=v["message"]["usage"]["cache_creation_input_tokens"].as_u64().unwrap_or(0);self.input=v["message"]["usage"]["input_tokens"].as_u64().unwrap_or(0)+self.cached+self.cache_write;chunks.push(self.chunk(json!({"role":"assistant"}),None));},
                "content_block_start" if v["content_block"]["type"]=="tool_use"=>chunks.push(self.chunk(json!({"tool_calls":[{"index":v["index"],"id":v["content_block"]["id"],"type":"function","function":{"name":v["content_block"]["name"],"arguments":""}}]}),None)),
                "content_block_delta" if v["delta"]["type"]=="text_delta"=>chunks.push(self.chunk(json!({"content":v["delta"]["text"]}),None)),
                "content_block_delta" if v["delta"]["type"]=="input_json_delta"=>chunks.push(self.chunk(json!({"tool_calls":[{"index":v["index"],"function":{"arguments":v["delta"]["partial_json"]}}]}),None)),
                "message_delta"=>{self.output=v["usage"]["output_tokens"].as_u64().unwrap_or(self.output);chunks.push(self.chunk(json!({}),v["delta"]["stop_reason"].as_str().map(|reason|match reason{"tool_use"=>"tool_calls","max_tokens"=>"length",_=>"stop"})));},_=>{},
            },
            Dialect::Gemini=>{
                let b=v.get("response").unwrap_or(&v);
                self.input=b["usageMetadata"]["promptTokenCount"].as_u64().unwrap_or(self.input);self.output=b["usageMetadata"]["candidatesTokenCount"].as_u64().unwrap_or(self.output);
                self.cached=b["usageMetadata"]["cachedContentTokenCount"].as_u64().unwrap_or(self.cached);
                if let Some(parts)=b.pointer("/candidates/0/content/parts").and_then(Value::as_array){for part in parts{
                    if let Some(t)=part["text"].as_str().filter(|_|part["thought"]!=true){chunks.push(self.chunk(json!({"content":t}),None));}
                    if let Some(f)=part.get("functionCall"){let index=self.source_tools;self.source_tools+=1;chunks.push(self.chunk(json!({"tool_calls":[{"index":index,"id":f.get("id").cloned().unwrap_or_else(||json!(format!("call_{:x}",rand::random::<u64>()))),"type":"function","function":{"name":f["name"],"arguments":f["args"].to_string()}}]}),None));}
                    if part.get("inlineData").is_some()||part.get("fileData").is_some(){return Err("translated Gemini output contains unsupported media".into());}
                }}
                if b["candidates"][0]["finishReason"].as_str().is_some(){chunks.push(self.chunk(json!({}),Some(if b["candidates"][0]["finishReason"]=="MAX_TOKENS"{"length"}else if self.source_tools>0{"tool_calls"}else{"stop"})));}
            }
        }
        Ok(chunks)
    }
    fn event(&mut self, kind: &str, mut value: Value) -> String {
        value["type"] = json!(kind);
        value["sequence_number"] = json!(self.seq);
        self.seq += 1;
        sse(Some(kind), &value)
    }
    fn responses_chunk(&mut self, v: &Value) -> Result<String, String> {
        let mut out = String::new();
        if !self.started {
            self.started = true;
            out.push_str(&self.event("response.created",json!({"response":{"id":self.id,"object":"response","status":"in_progress","model":self.model,"output":[]}})));
        }
        let delta = &v["choices"][0]["delta"];
        if let Some(t) = delta["content"].as_str().filter(|s| !s.is_empty()) {
            if self.text.is_empty() {
                out.push_str(&self.event("response.output_item.added",json!({"output_index":0,"item":{"id":"msg_douane","type":"message","role":"assistant","status":"in_progress","content":[]}})));
                out.push_str(&self.event("response.content_part.added",json!({"item_id":"msg_douane","output_index":0,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}})));
            }
            self.text.push_str(t);
            if self.text.len() > 8 * 1024 * 1024 {
                return Err("translated response exceeds 8 MiB".into());
            }
            out.push_str(&self.event(
                "response.output_text.delta",
                json!({"item_id":"msg_douane","output_index":0,"content_index":0,"delta":t}),
            ));
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for c in calls {
                let index = c["index"].as_u64().unwrap_or(0) as usize;
                if !self.tools.contains_key(&index) {
                    let id = c["id"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("call_{index}"));
                    let item = json!({"id":format!("fc_{index}"),"type":"function_call","call_id":id,"name":"","arguments":"","status":"in_progress"});
                    self.tools.insert(index, item.clone());
                }
                if let Some(id) = c["id"].as_str().filter(|id| !id.is_empty()) {
                    self.tools.get_mut(&index).unwrap()["call_id"] = json!(id);
                }
                if let Some(name) = c["function"]["name"].as_str() {
                    let item = self.tools.get_mut(&index).unwrap();
                    item["name"] = json!(format!("{}{name}", item["name"].as_str().unwrap_or("")));
                }
                if let Some(args) = c["function"]["arguments"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                {
                    let item = self.tools.get_mut(&index).unwrap();
                    let mut accumulated = item["arguments"].as_str().unwrap_or("").to_string();
                    accumulated.push_str(args);
                    if accumulated.len() > 8 * 1024 * 1024 {
                        return Err("tool arguments exceed 8 MiB".into());
                    }
                    item["arguments"] = json!(accumulated);
                }
            }
        }
        if let Some(reason) = v["choices"][0]["finish_reason"].as_str() {
            let mut items = vec![];
            if !self.text.is_empty() {
                let part = json!({"type":"output_text","text":self.text,"annotations":[]});
                out.push_str(&self.event("response.output_text.done",json!({"item_id":"msg_douane","output_index":0,"content_index":0,"text":self.text})));
                out.push_str(&self.event(
                    "response.content_part.done",
                    json!({"item_id":"msg_douane","output_index":0,"content_index":0,"part":part}),
                ));
                let item = json!({"id":"msg_douane","type":"message","role":"assistant","status":"completed","content":[part]});
                out.push_str(&self.event(
                    "response.output_item.done",
                    json!({"output_index":0,"item":item}),
                ));
                items.push(item);
            }
            let tool_start = items.len();
            for (position, (_, mut item)) in self.tools.clone().into_iter().enumerate() {
                let index = tool_start + position;
                if item["name"].as_str().is_none_or(|name| name.is_empty()) {
                    return Err("upstream tool call has no function name".into());
                }
                let arguments = item["arguments"].as_str().unwrap_or("");
                if !arguments.is_empty() && serde_json::from_str::<Value>(arguments).is_err() {
                    return Err("upstream tool call arguments are not JSON".into());
                }
                let mut start = item.clone();
                start["arguments"] = json!("");
                out.push_str(&self.event(
                    "response.output_item.added",
                    json!({"output_index":index,"item":start}),
                ));
                out.push_str(&self.event(
                    "response.function_call_arguments.delta",
                    json!({"item_id":item["id"],"output_index":index,"delta":item["arguments"]}),
                ));
                item["status"] = json!("completed");
                out.push_str(&self.event("response.function_call_arguments.done",json!({"item_id":item["id"],"output_index":index,"arguments":item["arguments"]})));
                out.push_str(&self.event(
                    "response.output_item.done",
                    json!({"output_index":index,"item":item}),
                ));
                items.push(item);
            }
            let status = if reason == "length" {
                "incomplete"
            } else {
                "completed"
            };
            out.push_str(&self.event(if reason=="length"{"response.incomplete"}else{"response.completed"},json!({"response":{"id":self.id,"object":"response","created_at":time::OffsetDateTime::now_utc().unix_timestamp(),"status":status,"model":self.model,"output":items,"error":null,"usage":{"input_tokens":self.input,"output_tokens":self.output,"total_tokens":self.input+self.output,"input_tokens_details":{"cached_tokens":self.cached,"cache_write_tokens":self.cache_write}}}})));
        }
        Ok(out)
    }
}
