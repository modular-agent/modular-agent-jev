//! HTTP client for the TypeSafe AI System One API.
//!
//! Kept independent of the module layer so the request/response types and
//! the retry loop can be exercised without a running agent.

use std::collections::BTreeMap;
use std::time::Duration;

use modular_agent_core::{Error, Result};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

pub const DEFAULT_API_BASE: &str = "https://api.typesafe.ai";
pub const DEFAULT_MODEL: &str = "jev-latest";

const MAX_RETRIES: u32 = 3;
const BASE_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const USER_AGENT: &str = concat!("modular-agent-jev/", env!("CARGO_PKG_VERSION"));

/// A question posed against the `state`, tagged by its `type`.
///
/// `instructions` accepts a string, object, array, or null; it is always sent
/// so a missing value reaches the API as an explicit `null`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// Yes/no style judgement answered with a probability in `0..=1`.
    Noul {
        instructions: Option<Json>,
        /// Optional `{ "true": ..., "false": ... }` descriptions of each side.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<Json>,
    },
    /// Pick one label out of `criteria` (label -> description or null).
    Choice {
        instructions: Option<Json>,
        criteria: serde_json::Map<String, Json>,
    },
    /// Place the state on an ordered scale described by 2..=10 entries.
    Score {
        instructions: Option<Json>,
        criteria: Vec<Json>,
    },
}

/// One answer, tagged by the question `type` it belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        confidence: f64,
        probabilities: Json,
    },
    Score {
        score: Json,
        confidence: f64,
        legend: Json,
        probabilities: Json,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemOneRequest {
    pub state: Json,
    pub questions: BTreeMap<String, Question>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemOneResponse {
    pub model: String,
    pub answers: BTreeMap<String, Answer>,
    pub usage: Usage,
}

#[derive(Debug, Clone)]
pub struct TypeSafeClient {
    http: Client,
    base_url: String,
    api_key: String,
}

impl TypeSafeClient {
    pub fn new(base_url: &str, api_key: &str) -> Result<Self> {
        let http = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| Error::IoError(format!("TypeSafe client build error: {e}")))?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        })
    }

    /// Calls `POST /v1/systemone`, retrying rate limits, overloads and
    /// connection failures with exponential backoff.
    pub async fn system_one(&self, request: &SystemOneRequest) -> Result<SystemOneResponse> {
        let url = format!("{}/v1/systemone", self.base_url);
        let mut attempt = 0;
        loop {
            let sent = self
                .http
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(request)
                .send()
                .await;

            let (error, retry_after) = match sent {
                Ok(response) if response.status().is_success() => {
                    return response.json().await.map_err(|e| {
                        Error::IoError(format!("System One response parse error: {e}"))
                    });
                }
                Ok(response) => {
                    let status = response.status();
                    let retry_after = parse_retry_after(response.headers());
                    let body = response.text().await.unwrap_or_default();
                    if !is_retryable_status(status) {
                        return Err(status_error(status, &body));
                    }
                    (status_error(status, &body), retry_after)
                }
                Err(e) if e.is_connect() || e.is_timeout() => (
                    Error::IoError(format!("System One is not reachable at {url}: {e}")),
                    None,
                ),
                Err(e) => return Err(Error::IoError(format!("System One request error: {e}"))),
            };

            if attempt >= MAX_RETRIES {
                return Err(error);
            }
            tokio::time::sleep(backoff_delay(attempt, retry_after)).await;
            attempt += 1;
        }
    }
}

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.as_u16() == 529
}

fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Server-provided `Retry-After` wins; otherwise `BASE_DELAY * 2^attempt`.
/// Both are capped at `MAX_DELAY`.
fn backoff_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    let delay = retry_after.unwrap_or_else(|| BASE_DELAY.saturating_mul(1u32 << attempt.min(16)));
    delay.min(MAX_DELAY)
}

fn status_error(status: StatusCode, body: &str) -> Error {
    let body = body.trim();
    let detail = if body.is_empty() {
        String::new()
    } else {
        format!(": {}", truncate(body, 1000))
    };
    match status {
        StatusCode::UNAUTHORIZED => Error::InvalidConfig(format!(
            "TypeSafe API key was rejected (401 Unauthorized){detail}"
        )),
        StatusCode::UNPROCESSABLE_ENTITY => Error::InvalidValue(format!(
            "System One rejected the request (422 Unprocessable Entity){detail}"
        )),
        _ => Error::IoError(format!("System One request failed ({status}){detail}")),
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars).collect();
        format!("{head}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_noul_question_with_and_without_criteria() {
        let bare = Question::Noul {
            instructions: Some(json!("Is this spam?")),
            criteria: None,
        };
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({ "type": "noul", "instructions": "Is this spam?" })
        );

        let with_criteria = Question::Noul {
            instructions: None,
            criteria: Some(json!({ "true": "spam", "false": "ham" })),
        };
        assert_eq!(
            serde_json::to_value(&with_criteria).unwrap(),
            json!({
                "type": "noul",
                "instructions": null,
                "criteria": { "true": "spam", "false": "ham" }
            })
        );
    }

    #[test]
    fn serializes_choice_and_score_questions() {
        let mut labels = serde_json::Map::new();
        labels.insert("positive".into(), Json::Null);
        labels.insert("negative".into(), json!("Complaints or anger"));
        let choice = Question::Choice {
            instructions: Some(json!({ "task": "sentiment" })),
            criteria: labels,
        };
        assert_eq!(
            serde_json::to_value(&choice).unwrap(),
            json!({
                "type": "choice",
                "instructions": { "task": "sentiment" },
                "criteria": { "positive": null, "negative": "Complaints or anger" }
            })
        );

        let score = Question::Score {
            instructions: Some(json!("Rate urgency")),
            criteria: vec![json!("low"), json!("medium"), json!("high")],
        };
        assert_eq!(
            serde_json::to_value(&score).unwrap(),
            json!({
                "type": "score",
                "instructions": "Rate urgency",
                "criteria": ["low", "medium", "high"]
            })
        );
    }

    #[test]
    fn deserializes_each_answer_kind() {
        let response: SystemOneResponse = serde_json::from_value(json!({
            "model": "jev-latest",
            "answers": {
                "spam": { "type": "noul", "noul": 0.93 },
                "tone": {
                    "type": "choice",
                    "choice": "negative",
                    "confidence": 0.81,
                    "probabilities": { "positive": 0.19, "negative": 0.81 }
                },
                "urgency": {
                    "type": "score",
                    "score": 2,
                    "confidence": 0.6,
                    "legend": ["low", "medium", "high"],
                    "probabilities": [0.1, 0.3, 0.6]
                }
            },
            "usage": { "input_tokens": 120, "output_tokens": 8 }
        }))
        .unwrap();

        assert_eq!(response.model, "jev-latest");
        assert_eq!(response.answers["spam"], Answer::Noul { noul: 0.93 });
        assert_eq!(
            response.answers["tone"],
            Answer::Choice {
                choice: "negative".into(),
                confidence: 0.81,
                probabilities: json!({ "positive": 0.19, "negative": 0.81 }),
            }
        );
        assert_eq!(
            response.answers["urgency"],
            Answer::Score {
                score: json!(2),
                confidence: 0.6,
                legend: json!(["low", "medium", "high"]),
                probabilities: json!([0.1, 0.3, 0.6]),
            }
        );
        assert_eq!(
            response.usage,
            Usage {
                input_tokens: 120,
                output_tokens: 8
            }
        );
    }

    #[test]
    fn request_passes_state_through_and_omits_missing_model() {
        let mut questions = BTreeMap::new();
        questions.insert(
            "answer".to_string(),
            Question::Noul {
                instructions: None,
                criteria: None,
            },
        );

        let text = SystemOneRequest {
            state: json!("hello"),
            questions: questions.clone(),
            model: None,
        };
        let json = serde_json::to_value(&text).unwrap();
        assert_eq!(json["state"], json!("hello"));
        assert!(json.get("model").is_none());

        let object = SystemOneRequest {
            state: json!({ "subject": "Hi", "body": ["a", "b"] }),
            questions,
            model: Some("jev-latest".into()),
        };
        let json = serde_json::to_value(&object).unwrap();
        assert_eq!(
            json["state"],
            json!({ "subject": "Hi", "body": ["a", "b"] })
        );
        assert_eq!(json["model"], json!("jev-latest"));
    }

    #[test]
    fn backoff_prefers_retry_after_and_caps_delay() {
        assert_eq!(backoff_delay(0, None), Duration::from_secs(1));
        assert_eq!(backoff_delay(2, None), Duration::from_secs(4));
        assert_eq!(backoff_delay(10, None), MAX_DELAY);
        assert_eq!(
            backoff_delay(0, Some(Duration::from_secs(7))),
            Duration::from_secs(7)
        );
        assert_eq!(backoff_delay(0, Some(Duration::from_secs(600))), MAX_DELAY);
    }

    #[test]
    fn maps_status_codes_to_error_kinds() {
        assert!(matches!(
            status_error(StatusCode::UNAUTHORIZED, ""),
            Error::InvalidConfig(_)
        ));
        assert!(matches!(
            status_error(StatusCode::UNPROCESSABLE_ENTITY, "{\"detail\":\"bad\"}"),
            Error::InvalidValue(msg) if msg.contains("bad")
        ));
        assert!(matches!(
            status_error(StatusCode::TOO_MANY_REQUESTS, ""),
            Error::IoError(_)
        ));
        assert!(is_retryable_status(StatusCode::from_u16(529).unwrap()));
        assert!(!is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
    }
}
