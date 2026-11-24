//! Gemini's OpenAI-compatible endpoint (ai.google.dev/gemini-api/docs/openai): it rejects
//! request fields it doesn't know, and Gemini 3 and later sign each turn's function calls with
//! a thought signature (`extra_content.google.thought_signature` on the call) that must come
//! back on the next request, or it answers 400. Every change here is made on the copy being
//! sent, the same way on every request, so the cached prefix still hits.

use serde_json::{Value, json};

/// The placeholder Google documents for calls that have no signature of their own, such as
/// calls another model made before the session switched to Gemini; it skips the check.
const NO_SIGNATURE: &str = "skip_thought_signature_validator";

/// Whether requests go to Google's Gemini API, on AI Studio or Vertex AI.
pub(super) fn is_gemini(base_url: &str) -> bool {
    let host = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest)
        .split(['/', ':'])
        .next()
        .unwrap_or_default();
    host == "generativelanguage.googleapis.com" || host.ends_with("aiplatform.googleapis.com")
}

/// For Gemini: an assistant message whose calls carry no signature gets the placeholder on
/// its first call, where Gemini puts a signature of its own. For any other endpoint: the
/// calls' `extra_content` is left out, since it is Gemini's and a strict API would refuse it.
pub(super) fn prepare_calls(messages: &mut [Value], gemini: bool) {
    for calls in messages
        .iter_mut()
        .filter_map(|message| message.get_mut("tool_calls"))
        .filter_map(Value::as_array_mut)
    {
        if !gemini {
            for call in calls.iter_mut().filter_map(Value::as_object_mut) {
                call.remove("extra_content");
            }
        } else if !calls.iter().any(|call| call.get("extra_content").is_some())
            && let Some(first) = calls.first_mut().and_then(Value::as_object_mut)
        {
            first.insert(
                "extra_content".into(),
                json!({"google": {"thought_signature": NO_SIGNATURE}}),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_is_told_apart_by_its_host() {
        assert!(is_gemini(
            "https://generativelanguage.googleapis.com/v1beta/openai"
        ));
        assert!(is_gemini(
            "https://us-central1-aiplatform.googleapis.com/v1/projects/p/locations/l/endpoints/openapi"
        ));
        assert!(!is_gemini("https://api.openai.com/v1"));
        assert!(!is_gemini(
            "https://evil.example/generativelanguage.googleapis.com"
        ));
        assert!(!is_gemini(""));
    }

    #[test]
    fn signatures_go_to_gemini_and_nowhere_else() {
        let signed = json!({"google": {"thought_signature": "sig-a"}});
        let history = vec![
            json!({"role": "user", "content": "go"}),
            json!({"role": "assistant", "tool_calls": [
                {"id": "a", "function": {"name": "read", "arguments": "{}"}, "extra_content": signed},
                {"id": "b", "function": {"name": "read", "arguments": "{}"}},
            ]}),
            json!({"role": "assistant", "tool_calls": [
                {"id": "c", "function": {"name": "bash", "arguments": "{}"}},
                {"id": "d", "function": {"name": "bash", "arguments": "{}"}},
            ]}),
            json!({"role": "assistant", "content": "done"}),
        ];

        let mut gemini = history.clone();
        prepare_calls(&mut gemini, true);
        assert_eq!(
            gemini[1], history[1],
            "a signed turn is sent as Gemini signed it"
        );
        assert_eq!(
            gemini[2]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
            NO_SIGNATURE
        );
        assert!(gemini[2]["tool_calls"][1].get("extra_content").is_none());
        let mut again = gemini.clone();
        prepare_calls(&mut again, true);
        assert_eq!(again, gemini, "the same copy every time");

        let mut other = history.clone();
        prepare_calls(&mut other, false);
        assert!(other[1]["tool_calls"][0].get("extra_content").is_none());
        assert_eq!(other[1]["tool_calls"][0]["id"], "a");
    }
}
