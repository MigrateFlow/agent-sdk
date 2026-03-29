use crate::error::{SdkError, SdkResult};
use crate::traits::llm_client::LlmClient;

/// Extract JSON from a string that might be wrapped in markdown code blocks.
pub fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();

    if let Some(start) = trimmed.find("```json") {
        let start = start + 7;
        if let Some(end) = trimmed[start..].find("```") {
            return trimmed[start..start + end].trim();
        }
    }

    if let Some(start) = trimmed.find("```") {
        let start = start + 3;
        let start = trimmed[start..]
            .find('\n')
            .map(|i| start + i + 1)
            .unwrap_or(start);
        if let Some(end) = trimmed[start..].find("```") {
            return trimmed[start..start + end].trim();
        }
    }

    trimmed
}

/// Parse a JSON string extracted from an LLM response.
pub fn parse_llm_json<T: serde::de::DeserializeOwned>(text: &str) -> SdkResult<T> {
    let json_str = extract_json(text);
    serde_json::from_str(json_str).map_err(|e| {
        SdkError::LlmResponseParse(format!(
            "Failed to parse JSON response: {}. Response text: {}",
            e,
            &text[..text.len().min(500)]
        ))
    })
}

/// Convenience: call an LlmClient and parse the response as JSON.
pub async fn ask_json<T: serde::de::DeserializeOwned>(
    client: &dyn LlmClient,
    system: &str,
    user_message: &str,
) -> SdkResult<(T, u64)> {
    let (text, tokens) = client.ask(system, user_message).await?;
    let parsed: T = parse_llm_json(&text)?;
    Ok((parsed, tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_raw() {
        let input = r#"{"key": "value"}"#;
        assert_eq!(extract_json(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_extract_json_code_block() {
        let input = "Here is the result:\n```json\n{\"key\": \"value\"}\n```\nDone.";
        assert_eq!(extract_json(input), r#"{"key": "value"}"#);
    }

    #[test]
    fn test_extract_json_generic_block() {
        let input = "```\n{\"key\": \"value\"}\n```";
        assert_eq!(extract_json(input), r#"{"key": "value"}"#);
    }
}
