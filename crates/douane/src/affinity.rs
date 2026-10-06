//! Bounded, idle-expiring account affinity. Whether a pin still holds is the
//! strategy's call, in `AppState::candidates`.

use axum::http::HeaderMap;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub fn session_key(headers: &HeaderMap, body: &Value) -> Option<String> {
    let header = [
        "session_id",
        "session-id",
        "x-opencode-session",
        "thread-id",
        "x-codex-thread-id",
        "x-claude-code-session-id",
    ]
    .iter()
    .find_map(|key| {
        headers
            .get(*key)
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
    });
    let metadata = body
        .pointer("/metadata/user_id")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    let legacy = body
        .pointer("/metadata/user_id")
        .and_then(Value::as_str)
        .and_then(|s| s.rsplit_once("_session_").map(|(_, id)| id));
    let id = header
        .or_else(|| body.get("prompt_cache_key").and_then(Value::as_str))
        .or_else(|| body.pointer("/metadata/session_id").and_then(Value::as_str))
        .or_else(|| metadata.as_ref().and_then(|v| v["session_id"].as_str()))
        .or(legacy);
    id.filter(|s| !s.is_empty())
        .map(|s| hex::encode(Sha256::digest(s.as_bytes())))
}

struct Bindings {
    entries: HashMap<(String, String), (String, Instant)>,
    limit: usize,
    ttl: Duration,
}
impl Default for Bindings {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            limit: 10000,
            ttl: Duration::from_secs(24 * 3600),
        }
    }
}
impl Bindings {
    fn get(&mut self, provider: &str, session: &str) -> Option<String> {
        let entry = self.entries.get_mut(&(provider.into(), session.into()))?;
        if entry.1.elapsed() > self.ttl {
            return None;
        }
        entry.1 = Instant::now();
        Some(entry.0.clone())
    }
    fn bind(&mut self, provider: &str, session: &str, account: &str) {
        let key = (provider.into(), session.into());
        if !self.entries.contains_key(&key) && self.entries.len() >= self.limit {
            self.entries.retain(|_, (_, t)| t.elapsed() <= self.ttl);
            if self.entries.len() >= self.limit {
                if let Some(key) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, (_, t))| *t)
                    .map(|(k, _)| k.clone())
                {
                    self.entries.remove(&key);
                }
            }
        }
        self.entries.insert(key, (account.into(), Instant::now()));
    }
}

#[derive(Default)]
pub struct Affinity {
    sessions: Bindings,
    // Response traffic must never evict a conversation's subscription pin.
    responses: Bindings,
}
impl Affinity {
    pub fn get(&mut self, provider: &str, session: &str) -> Option<String> {
        self.sessions.get(provider, session)
    }
    pub fn bind(&mut self, provider: &str, session: &str, account: &str) {
        self.sessions.bind(provider, session, account);
    }
    pub fn response_owner(&mut self, provider: &str, response: &str) -> Option<String> {
        let key = hex::encode(Sha256::digest(response.as_bytes()));
        self.responses.get(provider, &key)
    }
    pub fn bind_response(&mut self, provider: &str, response: &str, account: &str) {
        let key = hex::encode(Sha256::digest(response.as_bytes()));
        self.responses.bind(provider, &key, account);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn request_id_does_not_override_stable_session() {
        let mut h = HeaderMap::new();
        h.insert("session_id", "conversation".parse().unwrap());
        let a = session_key(&h, &json!({}));
        h.insert("x-client-request-id", "new-request".parse().unwrap());
        assert_eq!(a, session_key(&h, &json!({})));
    }
    #[test]
    fn provider_sessions_are_isolated_and_bindings_stay_stable() {
        let mut a = Affinity::default();
        a.bind("codex", "s", "a");
        a.bind("claude", "s", "b");
        assert_eq!(a.get("codex", "s"), Some("a".into()));
        assert_eq!(a.get("claude", "s"), Some("b".into()));
    }
    #[test]
    fn cache_key_and_native_session_use_same_identity() {
        let mut h = HeaderMap::new();
        h.insert("session_id", "same".parse().unwrap());
        assert_eq!(
            session_key(&h, &json!({})),
            session_key(&HeaderMap::new(), &json!({"prompt_cache_key":"same"}))
        );
    }
    #[test]
    fn response_flood_does_not_evict_conversation_pins() {
        let mut a = Affinity::default();
        a.responses.limit = 3;
        a.bind("codex", "conversation", "replacement");
        for n in 0..20 {
            a.bind_response("codex", &format!("response-{n}"), "replacement");
        }
        assert_eq!(a.responses.entries.len(), 3);
        assert_eq!(a.get("codex", "conversation"), Some("replacement".into()));
        assert_eq!(a.response_owner("codex", "response-0"), None);
        assert_eq!(
            a.response_owner("codex", "response-19"),
            Some("replacement".into())
        );
        assert_eq!(a.response_owner("openai-compatible", "response-19"), None);
    }
}
