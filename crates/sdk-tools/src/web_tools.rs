use async_trait::async_trait;
use reqwest::Url;
use scraper::{Html, Selector};
use serde_json::json;

use sdk_core::error::SdkResult;
use sdk_core::traits::tool::{Tool, ToolDefinition};

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn is_read_only(&self) -> bool { true }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_search".to_string(),
            description: "Search the public web for up-to-date information and return a compact list of result titles and URLs.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search query" },
                    "max_results": { "type": "integer", "description": "Maximum number of results to return (default: 5)" }
                },
                "required": ["query"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> SdkResult<serde_json::Value> {
        let query = arguments["query"].as_str().unwrap_or("").trim();
        let max_results = arguments["max_results"].as_u64().unwrap_or(5) as usize;

        if query.is_empty() {
            return Ok(json!({ "error": "Missing 'query' argument" }));
        }

        let mut url = Url::parse("https://html.duckduckgo.com/html/")
            .expect("hardcoded DuckDuckGo HTML URL should be valid");
        url.query_pairs_mut().append_pair("q", query);

        let client = reqwest::Client::builder()
            .user_agent("agent-sdk-web-search/0.1")
            .build()?;

        let html = client.get(url).send().await?.text().await?;
        let document = Html::parse_document(&html);
        let selector = Selector::parse("a.result__a")
            .expect("hardcoded search result selector should be valid");

        let mut results = Vec::new();

        for link in document.select(&selector).take(max_results) {
            let title = link.text().collect::<Vec<_>>().join(" ").trim().to_string();
            let href = link.value().attr("href").unwrap_or("").to_string();

            if title.is_empty() || href.is_empty() {
                continue;
            }

            results.push(json!({
                "title": title,
                "url": href,
            }));
        }

        Ok(json!({
            "query": query,
            "results": results,
            "count": results.len(),
        }))
    }
}
