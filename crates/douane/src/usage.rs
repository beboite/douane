//! Counting what actually travelled.
//!
//! douane prices nothing: a reseller's price list is fiction, so any cost this
//! program computed would be a precise number built on a made-up one. What it
//! does keep is the token counts themselves, because they are trust signals. A
//! cache-write count that dwarfs the reads says the upstream rewrites the whole
//! context every few calls, which is what a pool rotating one client across
//! accounts looks like, and no token total shows it.
//!
//! Two response shapes have to be handled. A buffered answer carries `usage` in
//! the JSON body. A streamed one spreads it across two SSE events: input and
//! cache counts land in `message_start`, output lands in `message_delta`, and
//! the last `message_delta` wins because the count is cumulative, not
//! incremental.

use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
}

impl Usage {
    /// Read whatever an Anthropic `usage` object holds.
    fn absorb(&mut self, usage: &serde_json::Value) {
        let n = |k: &str| usage.get(k).and_then(serde_json::Value::as_u64);
        if let Some(v) = n("input_tokens") {
            self.input_tokens = Some(v);
        }
        if let Some(v) = n("output_tokens") {
            self.output_tokens = Some(v);
        }
        if let Some(v) = n("cache_read_input_tokens") {
            self.cache_read_tokens = Some(v);
        }
        if let Some(v) = n("cache_creation_input_tokens") {
            self.cache_write_tokens = Some(v);
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// How the response carries its counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `text/event-stream`. Scanned line by line as it flies past, so a long
    /// generation costs a few hundred bytes of state rather than a copy of the
    /// answer.
    Sse,
    /// One JSON object. Has to be held until the end, since `usage` can be
    /// anywhere in it, so this path is capped.
    Json,
}

/// The most of a buffered body worth holding on to.
///
/// A real Anthropic response is a few tens of kilobytes. Anything past this is
/// not a body whose `usage` field is going to be found, and holding it would
/// mean one slow client could pin megabytes per in-flight call.
const JSON_CAP: usize = 4 * 1024 * 1024;

/// Accumulates usage from a response as it goes past.
pub struct Tap {
    mode: Mode,
    usage: Usage,
    /// In SSE mode, the tail of the last chunk when it stopped mid-line: frames
    /// are split on chunk boundaries all the time and a `data:` line cut in half
    /// parses as nothing at all. In JSON mode, the body so far.
    partial: String,
    truncated: bool,
    pub bytes: usize,
}

impl Tap {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            usage: Usage::default(),
            partial: String::new(),
            truncated: false,
            bytes: 0,
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        self.bytes += chunk.len();

        // Non-UTF-8 bytes mean this is not a response this module knows how to
        // read, so drop the buffer rather than let it grow on a binary body.
        let Ok(text) = std::str::from_utf8(chunk) else {
            self.partial.clear();
            self.truncated = true;
            return;
        };

        if self.mode == Mode::Json {
            if self.partial.len() + text.len() > JSON_CAP {
                self.truncated = true;
                self.partial.clear();
            }
            if !self.truncated {
                self.partial.push_str(text);
            }
            return;
        }

        self.partial.push_str(text);

        // Keep the last, possibly incomplete, line for the next chunk.
        let Some(cut) = self.partial.rfind('\n') else {
            // A single line longer than any real SSE frame is not one. Cap the
            // buffer so a malformed upstream cannot make this grow unbounded.
            if self.partial.len() > 1_000_000 {
                self.partial.clear();
            }
            return;
        };
        let complete: String = self.partial.drain(..=cut).collect();
        for line in complete.lines() {
            self.absorb_sse_line(line);
        }
    }

    /// Call once the response is over. Only JSON mode has anything left to do,
    /// since the object cannot be parsed until its last brace has arrived.
    pub fn finish(&mut self) {
        if self.mode == Mode::Json && !self.truncated {
            let body = std::mem::take(&mut self.partial);
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body)
                && let Some(u) = v.get("usage")
            {
                self.usage.absorb(u);
            }
        }
    }

    fn absorb_sse_line(&mut self, line: &str) {
        let Some(payload) = line.strip_prefix("data:") else {
            return;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(payload.trim()) else {
            return;
        };
        // `message_start` nests usage under `message`, `message_delta` puts it
        // at the top level. Look in both rather than switch on the event type,
        // which travels on a separate line and may not have been seen yet.
        if let Some(u) = v.get("usage") {
            self.usage.absorb(u);
        }
        if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
            self.usage.absorb(u);
        }
    }

    pub fn usage(&self) -> Usage {
        self.usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffered_body_gives_up_its_counts() {
        let mut t = Tap::new(Mode::Json);
        // Arriving in two pieces, the way a real body does.
        t.feed(br#"{"id":"msg_1","usage":{"input_tokens":1200,"output_tokens":88,"#);
        t.feed(br#""cache_read_input_tokens":900,"cache_creation_input_tokens":10}}"#);
        assert!(
            t.usage().is_empty(),
            "nothing is known until the last brace"
        );
        t.finish();
        let u = t.usage();
        assert_eq!(u.input_tokens, Some(1200));
        assert_eq!(u.output_tokens, Some(88));
        assert_eq!(u.cache_read_tokens, Some(900));
        assert_eq!(u.cache_write_tokens, Some(10));
    }

    #[test]
    fn a_stream_is_read_across_its_two_events() {
        let mut t = Tap::new(Mode::Sse);
        t.feed(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":500,\"output_tokens\":1}}}\n\n");
        t.feed(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":42}}\n\n");
        let u = t.usage();
        assert_eq!(u.input_tokens, Some(500));
        // The last delta wins: Anthropic sends a cumulative count, so taking the
        // latest is right and summing them would roughly double the bill.
        assert_eq!(u.output_tokens, Some(42));
    }

    #[test]
    fn a_frame_split_across_chunks_still_parses() {
        let mut t = Tap::new(Mode::Sse);
        t.feed(b"data: {\"usage\":{\"input_t");
        assert_eq!(t.usage().input_tokens, None);
        t.feed(b"okens\":77}}\n");
        assert_eq!(t.usage().input_tokens, Some(77));
        assert_eq!(t.bytes, 24 + 12);
    }

    #[test]
    fn junk_never_makes_the_buffer_grow_without_end() {
        let mut t = Tap::new(Mode::Sse);
        for _ in 0..40 {
            t.feed(&vec![b'x'; 50_000]);
        }
        assert!(t.partial.len() <= 1_000_000);
        assert!(t.usage().is_empty());
    }

    #[test]
    fn an_oversized_json_body_is_dropped_rather_than_held() {
        let mut t = Tap::new(Mode::Json);
        for _ in 0..5 {
            t.feed(&vec![b'x'; 1024 * 1024]);
        }
        t.finish();
        assert!(t.partial.is_empty());
        assert!(t.truncated);
        // The byte count is still exact: only the parse is given up on.
        assert_eq!(t.bytes, 5 * 1024 * 1024);
    }
}
