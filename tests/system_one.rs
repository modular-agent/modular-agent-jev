//! Network integration tests against the live System One API.
//!
//! They spend real quota and need `TYPESAFE_API_KEY`, so they are ignored by
//! default: `TYPESAFE_API_KEY=... cargo test -p modular-agent-jev -- --ignored`

use std::collections::BTreeMap;
use std::env;

use modular_agent_jev::client::{
    Answer, DEFAULT_API_BASE, DEFAULT_MODEL, Question, SystemOneRequest, TypeSafeClient,
};
use serde_json::json;

fn client() -> TypeSafeClient {
    let api_key = env::var("TYPESAFE_API_KEY").expect("TYPESAFE_API_KEY must be set");
    let base = env::var("TYPESAFE_API_BASE").unwrap_or_else(|_| DEFAULT_API_BASE.to_string());
    TypeSafeClient::new(&base, &api_key).expect("failed to build client")
}

#[tokio::test]
#[ignore = "requires TYPESAFE_API_KEY and network access to api.typesafe.ai"]
async fn answers_all_three_question_kinds() {
    let mut labels = serde_json::Map::new();
    labels.insert("complaint".into(), json!("The customer is unhappy"));
    labels.insert("praise".into(), json!(null));
    labels.insert("question".into(), json!(null));

    let mut questions = BTreeMap::new();
    questions.insert(
        "refund".to_string(),
        Question::Noul {
            instructions: Some(json!("Is the sender asking for a refund?")),
            criteria: None,
        },
    );
    questions.insert(
        "kind".to_string(),
        Question::Choice {
            instructions: Some(json!("What kind of message is this?")),
            criteria: labels,
        },
    );
    questions.insert(
        "urgency".to_string(),
        Question::Score {
            instructions: Some(json!("How urgent is this message?")),
            criteria: vec![json!("can wait"), json!("this week"), json!("today")],
        },
    );

    let request = SystemOneRequest {
        state: json!("My order arrived broken and I want my money back today."),
        questions,
        model: Some(DEFAULT_MODEL.to_string()),
    };

    let response = client()
        .system_one(&request)
        .await
        .unwrap_or_else(|e| panic!("system_one failed: {e}"));

    assert!(!response.model.is_empty());
    assert!(response.usage.input_tokens > 0);

    match &response.answers["refund"] {
        Answer::Noul { noul } => assert!((0.0..=1.0).contains(noul) && *noul > 0.5),
        other => panic!("unexpected refund answer: {other:?}"),
    }
    match &response.answers["kind"] {
        Answer::Choice {
            choice, confidence, ..
        } => {
            assert_eq!(choice, "complaint");
            assert!((0.0..=1.0).contains(confidence));
        }
        other => panic!("unexpected kind answer: {other:?}"),
    }
    match &response.answers["urgency"] {
        Answer::Score {
            confidence, legend, ..
        } => {
            assert!((0.0..=1.0).contains(confidence));
            assert!(legend.is_array() || legend.is_object());
        }
        other => panic!("unexpected urgency answer: {other:?}"),
    }
}
