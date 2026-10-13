//! Capability-driven thinking controls for native Antigravity generation requests.

use crate::translate::Dialect;
use serde_json::{Map, Value, json};

#[derive(Clone, Copy)]
enum Amount {
    Budget(i64),
    Level(&'static str),
}

#[derive(Default)]
struct Controls {
    amount: Option<Amount>,
    visible: Option<bool>,
}

struct Support {
    min: i64,
    max: i64,
    levels: Vec<&'static str>,
    zero_allowed: bool,
    dynamic_allowed: bool,
}

/// Apply client thinking intent before the native request is wrapped for Antigravity.
/// Validation finishes before any mutation, so a refusal leaves the request intact.
pub(crate) fn configure(
    request: &mut Value,
    original: &Value,
    from: Dialect,
    model: &str,
    capabilities: Option<&Value>,
) -> Result<(), String> {
    configure_inner(request, original, from, model, capabilities)
        .map_err(|error| format!("Antigravity model {model}: {error}"))
}

fn configure_inner(
    request: &mut Value,
    original: &Value,
    from: Dialect,
    model: &str,
    capabilities: Option<&Value>,
) -> Result<(), String> {
    let controls = extract(original, from)?;
    if controls.amount.is_none() && controls.visible.is_none() {
        return Ok(());
    }
    let support = support(capabilities)?;
    let source_generation = request
        .get("generationConfig")
        .or_else(|| request.get("generation_config"));
    let mut generation = match source_generation {
        Some(value) => value
            .as_object()
            .cloned()
            .ok_or("generationConfig must be an object")?,
        None => Map::new(),
    };
    let amount = controls
        .amount
        .map(|amount| resolve(amount, &support, from, model, &generation))
        .transpose()?;
    // On a level-only model, a permitted zero removes the amount configuration.
    // Keeping includeThoughts alone would re-enable the upstream default.
    if matches!(amount, Some(None)) {
        generation.remove("thinkingConfig");
        generation.remove("thinking_config");
    } else {
        let mut thinking = match generation
            .get("thinkingConfig")
            .or_else(|| generation.get("thinking_config"))
        {
            Some(value) => value
                .as_object()
                .cloned()
                .ok_or("generationConfig.thinkingConfig must be an object")?,
            None => Map::new(),
        };
        let visible = controls.visible.or_else(|| {
            thinking
                .get("includeThoughts")
                .or_else(|| thinking.get("include_thoughts"))
                .and_then(Value::as_bool)
        });
        for key in ["thinking_budget", "thinking_level", "include_thoughts"] {
            thinking.remove(key);
        }
        if let Some(Some(amount)) = amount {
            thinking.remove("thinkingLevel");
            thinking.remove("thinkingBudget");
            match amount {
                Amount::Budget(budget) => {
                    thinking.insert("thinkingBudget".into(), json!(budget));
                }
                Amount::Level(level) => {
                    thinking.insert("thinkingLevel".into(), json!(level));
                }
            }
        }
        if let Some(visible) = visible {
            thinking.insert("includeThoughts".into(), json!(visible));
        }
        generation.remove("thinking_config");
        generation.insert("thinkingConfig".into(), Value::Object(thinking));
    }
    let object = request
        .as_object_mut()
        .ok_or("native generation request must be an object")?;
    object.remove("generation_config");
    object.insert("generationConfig".into(), Value::Object(generation));
    Ok(())
}

fn extract(original: &Value, from: Dialect) -> Result<Controls, String> {
    match from {
        Dialect::Messages => extract_messages(original),
        Dialect::Chat | Dialect::Responses => {
            let value = if from == Dialect::Chat {
                original.get("reasoning_effort")
            } else {
                original.pointer("/reasoning/effort")
            };
            let amount = value.map(effort).transpose()?.flatten();
            let summary = original
                .pointer("/reasoning/summary")
                .or_else(|| original.pointer("/reasoning/generate_summary"));
            let visible = summary
                .map(summary_visibility)
                .transpose()?
                .or_else(|| amount.map(|amount| !matches!(amount, Amount::Budget(0))));
            Ok(Controls { amount, visible })
        }
        Dialect::Gemini => {
            let thinking = original
                .get("generationConfig")
                .or_else(|| original.get("generation_config"))
                .and_then(|generation| {
                    generation
                        .get("thinkingConfig")
                        .or_else(|| generation.get("thinking_config"))
                });
            let Some(thinking) = thinking else {
                return Ok(Controls::default());
            };
            if !thinking.is_object() {
                return Err("generationConfig.thinkingConfig must be an object".into());
            }
            let amount = if let Some(level) = thinking
                .get("thinkingLevel")
                .or_else(|| thinking.get("thinking_level"))
            {
                effort(level)?
            } else {
                thinking
                    .get("thinkingBudget")
                    .or_else(|| thinking.get("thinking_budget"))
                    .map(budget)
                    .transpose()?
            };
            let visible = thinking
                .get("includeThoughts")
                .or_else(|| thinking.get("include_thoughts"))
                .map(|value| value.as_bool().ok_or("includeThoughts must be a boolean"))
                .transpose()?;
            Ok(Controls { amount, visible })
        }
    }
}

fn extract_messages(original: &Value) -> Result<Controls, String> {
    let Some(thinking) = original.get("thinking") else {
        return Ok(Controls::default());
    };
    if !thinking.is_object() {
        return Err("thinking must be an object".into());
    }
    let kind = thinking
        .get("type")
        .map(|value| value.as_str().ok_or("thinking.type must be a string"))
        .transpose()?
        .unwrap_or("");
    let amount = match kind {
        "disabled" => Some(Amount::Budget(0)),
        "enabled" | "" => {
            if let Some(value) = thinking.get("budget_tokens") {
                Some(budget(value)?)
            } else if kind == "enabled" {
                original
                    .pointer("/output_config/effort")
                    .map(effort)
                    .transpose()?
                    .flatten()
                    .or(Some(Amount::Budget(-1)))
            } else {
                None
            }
        }
        "adaptive" | "auto" => original
            .pointer("/output_config/effort")
            .map(effort)
            .transpose()?
            .flatten(),
        _ => return Err("thinking.type must be enabled, adaptive, auto, or disabled".into()),
    };
    let visible = if matches!(kind, "enabled" | "adaptive") {
        thinking
            .get("display")
            .map(|value| match value.as_str() {
                Some("summarized") => Ok(true),
                Some("omitted") => Ok(false),
                _ => Err("thinking.display must be summarized or omitted"),
            })
            .transpose()?
    } else {
        None
    };
    Ok(Controls { amount, visible })
}

const LEVELS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];

fn effort(value: &Value) -> Result<Option<Amount>, String> {
    let value = value
        .as_str()
        .ok_or("thinking effort must be a string")?
        .trim()
        .to_ascii_lowercase();
    match value.as_str() {
        "" => Ok(None),
        "none" => Ok(Some(Amount::Budget(0))),
        "auto" => Ok(Some(Amount::Budget(-1))),
        other => LEVELS
            .iter()
            .find(|level| **level == other)
            .map(|level| Some(Amount::Level(level)))
            .ok_or_else(|| {
                "thinking effort must be none, auto, minimal, low, medium, high, xhigh, or max"
                    .into()
            }),
    }
}

fn budget(value: &Value) -> Result<Amount, String> {
    let budget = value
        .as_i64()
        .filter(|budget| *budget >= -1)
        .ok_or("thinking budget must be an integer at least -1")?;
    Ok(Amount::Budget(budget))
}

fn summary_visibility(value: &Value) -> Result<bool, String> {
    match value.as_str().map(str::trim) {
        Some("none") => Ok(false),
        Some("auto" | "concise" | "detailed") => Ok(true),
        _ => Err("reasoning.summary must be none, auto, concise, or detailed".into()),
    }
}

fn support(value: Option<&Value>) -> Result<Support, String> {
    let value = value
        .filter(|value| value.is_object())
        .ok_or("thinking controls require advertised model thinking capabilities")?;
    let bound = |key| -> Result<i64, String> {
        value
            .get(key)
            .map(|value| {
                value
                    .as_i64()
                    .filter(|value| *value >= 0)
                    .ok_or_else(|| format!("capabilities.{key} must be a nonnegative integer"))
            })
            .transpose()
            .map(|value| value.unwrap_or(0))
    };
    let flag = |key| -> Result<bool, String> {
        value
            .get(key)
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| format!("capabilities.{key} must be a boolean"))
            })
            .transpose()
            .map(|value| value.unwrap_or(false))
    };
    let min = bound("min")?;
    let max = bound("max")?;
    if max > 0 && max < min {
        return Err("capabilities.max must be at least capabilities.min".into());
    }
    let mut levels = Vec::new();
    if let Some(value) = value.get("levels") {
        for value in value
            .as_array()
            .ok_or("capabilities.levels must be an array")?
        {
            match effort(value)? {
                Some(Amount::Level(level)) => levels.push(level),
                _ => return Err("capabilities.levels must contain named thinking levels".into()),
            }
        }
    }
    let zero_allowed = flag("zero_allowed")?;
    let dynamic_allowed = flag("dynamic_allowed")?;
    if min == 0 && max == 0 && levels.is_empty() && !zero_allowed && !dynamic_allowed {
        return Err("model advertises no supported thinking controls".into());
    }
    Ok(Support {
        min,
        max,
        levels,
        zero_allowed,
        dynamic_allowed,
    })
}

fn closest_level(level: &str, support: &Support) -> Result<&'static str, String> {
    let position = LEVELS
        .iter()
        .position(|candidate| *candidate == level)
        .unwrap();
    support
        .levels
        .iter()
        .filter_map(|candidate| {
            let index = LEVELS.iter().position(|level| level == candidate)?;
            Some(((position.abs_diff(index), index), *candidate))
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, level)| level)
        .ok_or_else(|| "model has no supported thinking levels".into())
}

fn resolve(
    mut amount: Amount,
    support: &Support,
    from: Dialect,
    model: &str,
    generation: &Map<String, Value>,
) -> Result<Option<Amount>, String> {
    if matches!(amount, Amount::Budget(-1)) && !support.dynamic_allowed {
        amount = if support.min == 0 && support.max == 0 && !support.levels.is_empty() {
            Amount::Level(closest_level("medium", support)?)
        } else {
            // Same bounded midpoint rule as the inspected Antigravity compatibility path.
            Amount::Budget(support.min + (support.max.saturating_sub(support.min) / 2))
        };
    }
    if matches!(amount, Amount::Budget(0)) && !support.levels.is_empty() {
        if support.zero_allowed {
            return Ok(None);
        }
        return closest_level("minimal", support).map(|level| Some(Amount::Level(level)));
    }
    if let Amount::Level(level) = amount {
        if !support.levels.is_empty() {
            if from == Dialect::Gemini && !support.levels.contains(&level) {
                return Err(format!(
                    "thinking level {level} is not advertised for this model"
                ));
            }
            return closest_level(level, support).map(|level| Some(Amount::Level(level)));
        }
        // Established cross-protocol effort budgets, not model-specific estimates.
        amount = Amount::Budget(match level {
            "minimal" => 512,
            "low" => 1024,
            "medium" => 8192,
            "high" => 24576,
            "xhigh" => 32768,
            "max" => 128000,
            _ => unreachable!(),
        });
    }
    let Amount::Budget(mut budget) = amount else {
        unreachable!()
    };
    if budget > 0 && support.min == 0 && support.max == 0 && !support.levels.is_empty() {
        let level = match budget {
            1..=512 => "minimal",
            513..=1024 => "low",
            1025..=8192 => "medium",
            8193..=24576 => "high",
            _ => "xhigh",
        };
        return closest_level(level, support).map(|level| Some(Amount::Level(level)));
    }
    if budget >= 0 && !(budget == 0 && support.zero_allowed) {
        budget = budget.max(support.min);
        if support.max > 0 {
            budget = budget.min(support.max);
        }
        if budget == 0 && !support.zero_allowed {
            return Err("model cannot disable thinking and advertises no minimum budget".into());
        }
    }
    if budget > 0 && model.to_ascii_lowercase().contains("claude") {
        if let Some(value) = generation.get("maxOutputTokens") {
            let limit = value
                .as_i64()
                .filter(|limit| *limit > 0)
                .ok_or("generationConfig.maxOutputTokens must be a positive integer")?;
            budget = budget.min(limit - 1);
            if budget <= 0 || budget < support.min {
                return Err(
                    "thinking budget cannot fit below maxOutputTokens and above model minimum"
                        .into(),
                );
            }
        }
    }
    Ok(Some(Amount::Budget(budget)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn budget_capabilities() -> Value {
        json!({"min":1024,"max":63999,"dynamic_allowed":false,"zero_allowed":false})
    }

    fn level_capabilities() -> Value {
        json!({"levels":["minimal","low","medium","high"],"zero_allowed":true,"dynamic_allowed":true})
    }

    #[test]
    fn messages_budget_and_visibility_survive_without_overwriting_generation_settings() {
        let mut request = json!({"generationConfig":{"maxOutputTokens":10000,"temperature":0.3}});
        configure(
            &mut request,
            &json!({"thinking":{"type":"enabled","budget_tokens":4096,"display":"summarized"}}),
            Dialect::Messages,
            "claude-sonnet-4-6",
            Some(&budget_capabilities()),
        )
        .unwrap();
        assert_eq!(
            request,
            json!({"generationConfig":{"maxOutputTokens":10000,"temperature":0.3,"thinkingConfig":{"thinkingBudget":4096,"includeThoughts":true}}})
        );
    }

    #[test]
    fn responses_effort_uses_advertised_level_and_visible_summary_default() {
        let mut request = json!({});
        configure(
            &mut request,
            &json!({"reasoning":{"effort":" high "}}),
            Dialect::Responses,
            "gemini-level",
            Some(&level_capabilities()),
        )
        .unwrap();
        assert_eq!(
            request["generationConfig"]["thinkingConfig"],
            json!({"thinkingLevel":"high","includeThoughts":true})
        );
    }

    #[test]
    fn chat_effort_uses_sourced_budget_mapping_when_no_levels_are_advertised() {
        let mut request = json!({});
        configure(
            &mut request,
            &json!({"reasoning_effort":"high"}),
            Dialect::Chat,
            "gemini-3.8-flash-high",
            Some(&json!({"min":32,"max":65535,"dynamic_allowed":true})),
        )
        .unwrap();
        assert_eq!(
            request["generationConfig"]["thinkingConfig"],
            json!({"thinkingBudget":24576,"includeThoughts":true})
        );
    }

    #[test]
    fn claude_budget_stays_below_output_limit() {
        let mut request = json!({"generationConfig":{"maxOutputTokens":4096}});
        configure(
            &mut request,
            &json!({"thinking":{"type":"enabled","budget_tokens":5000}}),
            Dialect::Messages,
            "claude-sonnet-4-6",
            Some(&budget_capabilities()),
        )
        .unwrap();
        assert_eq!(
            request["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            4095
        );
    }

    #[test]
    fn effort_and_auto_budgets_follow_model_bounds() {
        for (effort, expected) in [("high", 8192), ("auto", 4608)] {
            let mut request = json!({});
            configure(
                &mut request,
                &json!({"reasoning_effort":effort}),
                Dialect::Chat,
                "claude-budget",
                Some(&json!({"min":1024,"max":8192,"dynamic_allowed":false})),
            )
            .unwrap();
            assert_eq!(
                request["generationConfig"]["thinkingConfig"]["thinkingBudget"],
                expected
            );
        }
    }

    #[test]
    fn budget_takes_precedence_over_messages_output_effort() {
        let mut request = json!({});
        configure(&mut request, &json!({"thinking":{"type":"enabled","budget_tokens":4096,"display":"omitted"},"output_config":{"effort":"high"}}), Dialect::Messages, "claude-budget", Some(&budget_capabilities())).unwrap();
        assert_eq!(
            request["generationConfig"]["thinkingConfig"],
            json!({"thinkingBudget":4096,"includeThoughts":false})
        );
    }

    #[test]
    fn explicit_summary_omission_wins_over_effort_visibility_default() {
        let mut request = json!({});
        configure(
            &mut request,
            &json!({"reasoning":{"effort":"high","summary":"none"}}),
            Dialect::Responses,
            "gemini-level",
            Some(&level_capabilities()),
        )
        .unwrap();
        assert_eq!(
            request["generationConfig"]["thinkingConfig"]["includeThoughts"],
            false
        );
    }

    #[test]
    fn native_gemini_budget_and_explicit_visibility_are_canonicalized() {
        let original = json!({"generationConfig":{"thinkingConfig":{"thinking_budget":4096,"include_thoughts":false}}});
        let mut request = original.clone();
        configure(
            &mut request,
            &original,
            Dialect::Gemini,
            "gemini-budget",
            Some(&json!({"min":32,"max":65535,"dynamic_allowed":true})),
        )
        .unwrap();
        assert_eq!(
            request["generationConfig"]["thinkingConfig"],
            json!({"thinkingBudget":4096,"includeThoughts":false})
        );
    }

    #[test]
    fn omitted_controls_and_unqualified_adaptive_preserve_upstream_defaults() {
        for original in [json!({}), json!({"thinking":{"type":"adaptive"}})] {
            let mut request = json!({"generationConfig":{"temperature":0.3},"contents":[{"role":"user","parts":[{"text":"hi"}]}]});
            let expected = request.clone();
            configure(&mut request, &original, Dialect::Messages, "unknown", None).unwrap();
            assert_eq!(request, expected);
        }
    }

    #[test]
    fn invalid_or_unsupported_controls_refuse_without_mutating_request() {
        for (original, from, caps) in [
            (
                json!({"reasoning_effort":"banana"}),
                Dialect::Chat,
                Some(budget_capabilities()),
            ),
            (
                json!({"thinking":{"type":"enabled","budget_tokens":-2}}),
                Dialect::Messages,
                Some(budget_capabilities()),
            ),
            (
                json!({"reasoning":{"effort":"high"}}),
                Dialect::Responses,
                None,
            ),
            (
                json!({"reasoning_effort":42}),
                Dialect::Chat,
                Some(budget_capabilities()),
            ),
        ] {
            let mut request = json!({"generationConfig":{"temperature":0.3}});
            let expected = request.clone();
            assert!(
                configure(
                    &mut request,
                    &original,
                    from,
                    "fixture-model",
                    caps.as_ref()
                )
                .is_err()
            );
            assert_eq!(request, expected);
        }
    }

    #[test]
    fn level_model_converts_numeric_budget_and_removes_conflicting_fields() {
        let mut request = json!({"generationConfig":{"thinkingConfig":{"thinkingBudget":100,"thinking_level":"low"}}});
        configure(
            &mut request,
            &json!({"thinking":{"type":"enabled","budget_tokens":4096}}),
            Dialect::Messages,
            "gemini-level",
            Some(&level_capabilities()),
        )
        .unwrap();
        assert_eq!(
            request["generationConfig"]["thinkingConfig"],
            json!({"thinkingLevel":"medium"})
        );
    }

    #[test]
    fn zero_allowed_budget_can_disable_thinking() {
        let mut request = json!({});
        configure(
            &mut request,
            &json!({"thinking":{"type":"disabled","budget_tokens":4096}}),
            Dialect::Messages,
            "gemini-budget",
            Some(&json!({"min":32,"max":65535,"zero_allowed":true})),
        )
        .unwrap();
        assert_eq!(
            request["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            0
        );
    }
}
