use std::collections::BTreeMap;
use std::env;
use std::sync::{Mutex, OnceLock};

use modular_agent_core::{
    AsModule, Error, ModularAgent, Module, ModuleContext, ModuleData, ModuleOutput, ModuleSpec,
    Result, Value, async_trait, modular_agent,
};
use serde_json::Value as Json;

use crate::client::{
    DEFAULT_API_BASE, DEFAULT_MODEL, Question, SystemOneRequest, SystemOneResponse, TypeSafeClient,
};

static CATEGORY: &str = "Jev";

static PORT_STATE: &str = "state";
static PORT_ANSWER: &str = "answer";
static PORT_ANSWERS: &str = "answers";

static CONFIG_QUESTIONS: &str = "questions";
static CONFIG_MODEL: &str = "model";
static CONFIG_INSTRUCTIONS: &str = "instructions";
static CONFIG_CRITERIA: &str = "criteria";
static CONFIG_TYPESAFE_API_KEY: &str = "typesafe_api_key";
static CONFIG_TYPESAFE_API_BASE: &str = "typesafe_api_base";

/// Name under which single-question modules submit their one question.
const SINGLE_QUESTION: &str = "answer";

// ---------------------------------------------------------------------------
// Client caching (keyed by base URL + API key)
// ---------------------------------------------------------------------------

static CLIENT_MAP: OnceLock<Mutex<BTreeMap<String, TypeSafeClient>>> = OnceLock::new();

fn get_client_map() -> &'static Mutex<BTreeMap<String, TypeSafeClient>> {
    CLIENT_MAP.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn get_cached_client(base_url: &str, api_key: &str) -> Result<TypeSafeClient> {
    let key = format!("{}\0{}", base_url, api_key);
    let mut map = get_client_map().lock().unwrap();
    if let Some(client) = map.get(&key) {
        return Ok(client.clone());
    }
    let client = TypeSafeClient::new(base_url, api_key)?;
    map.insert(key, client.clone());
    Ok(client)
}

// ---------------------------------------------------------------------------
// Global config helpers
// ---------------------------------------------------------------------------

fn get_api_key(ma: &ModularAgent) -> Result<String> {
    if let Some(global_key) = ma
        .get_global_configs(SystemOneModule::DEF_NAME)
        .and_then(|cfg| cfg.get_string(CONFIG_TYPESAFE_API_KEY).ok())
        .filter(|key| !key.is_empty())
    {
        Ok(global_key)
    } else {
        env::var("TYPESAFE_API_KEY").map_err(|_| {
            Error::InvalidConfig(
                "TypeSafe API key is not set (global config or TYPESAFE_API_KEY)".to_string(),
            )
        })
    }
}

fn get_api_base(ma: &ModularAgent) -> String {
    ma.get_global_configs(SystemOneModule::DEF_NAME)
        .and_then(|cfg| cfg.get_string(CONFIG_TYPESAFE_API_BASE).ok())
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string())
}

fn build_client(ma: &ModularAgent) -> Result<TypeSafeClient> {
    let api_key = get_api_key(ma)?;
    let base_url = get_api_base(ma);
    get_cached_client(&base_url, &api_key)
}

// ---------------------------------------------------------------------------
// Shared request helpers
// ---------------------------------------------------------------------------

fn model_or_default(model: String) -> String {
    if model.is_empty() {
        DEFAULT_MODEL.to_string()
    } else {
        model
    }
}

fn instructions_from(text: String) -> Option<Json> {
    if text.trim().is_empty() {
        None
    } else {
        Some(Json::String(text))
    }
}

async fn call_system_one(
    ma: &ModularAgent,
    state: &Value,
    questions: BTreeMap<String, Question>,
    model: String,
) -> Result<SystemOneResponse> {
    let client = build_client(ma)?;
    let request = SystemOneRequest {
        state: state.to_json(),
        questions,
        model: Some(model_or_default(model)),
    };
    client.system_one(&request).await
}

async fn ask_single(
    ma: &ModularAgent,
    state: &Value,
    question: Question,
    model: String,
) -> Result<Value> {
    let mut questions = BTreeMap::new();
    questions.insert(SINGLE_QUESTION.to_string(), question);
    let mut response = call_system_one(ma, state, questions, model).await?;
    let answer = response.answers.remove(SINGLE_QUESTION).ok_or_else(|| {
        Error::IoError(format!(
            "System One response has no \"{SINGLE_QUESTION}\" answer"
        ))
    })?;
    to_value(&answer)
}

fn to_value<T: serde::Serialize>(value: &T) -> Result<Value> {
    let json = serde_json::to_value(value)
        .map_err(|e| Error::IoError(format!("Failed to convert answer: {e}")))?;
    Value::from_json(json)
}

// ---------------------------------------------------------------------------
// SystemOneModule
// ---------------------------------------------------------------------------

/// Asks TypeSafe AI's System One (Jev) several questions about one input at once.
///
/// The value on `state` is sent as-is (string, object, or array) together with
/// the `questions` map, and the whole API response comes back on `answers`.
/// Use this module when one input needs multiple judgements in a single round
/// trip; the `Noul`, `Choice`, and `Score` modules cover the single-question
/// case without hand-written JSON.
///
/// Each entry in `questions` is keyed by a name of your choosing and carries a
/// `type` of `noul`, `choice`, or `score`, optional `instructions`, and the
/// `criteria` that type expects:
/// - `noul`: optional `{ "true": ..., "false": ... }`
/// - `choice`: `{ "<label>": <description or null>, ... }`
/// - `score`: an ordered array of 2 to 10 entries
///
/// Rate limits and overloads are retried with exponential backoff, honouring
/// `Retry-After`. A rejected API key surfaces as a configuration error and an
/// invalid request (HTTP 422) as a value error carrying the API's message.
///
/// # Ports
/// - Input `state`: The content to evaluate; string, object, or array
/// - Output `answers`: The full response object: `model`, `answers` (keyed like
///   `questions`, each with a `type` field), and `usage`
///
/// # Configuration
/// - `questions`: Map of question name to question object
/// - `model`: Model name (default: "jev-latest")
///
/// # Global Configuration
/// - `typesafe_api_key`: TypeSafe API key; falls back to the `TYPESAFE_API_KEY`
///   environment variable when empty
/// - `typesafe_api_base`: API base URL (default: "https://api.typesafe.ai")
///
/// # Example
/// With `questions` set to
/// `{"spam": {"type": "noul", "instructions": "Is this spam?"}}` and input
/// `"Free prizes, click now!"`, outputs
/// `{"model": "jev-latest", "answers": {"spam": {"type": "noul", "noul": 0.97}}, "usage": {...}}`.
#[modular_agent(
    title = "System One",
    category = CATEGORY,
    inputs = [PORT_STATE],
    outputs = [PORT_ANSWERS],
    object_config(name = CONFIG_QUESTIONS),
    string_config(name = CONFIG_MODEL, default = DEFAULT_MODEL, detail),
    custom_global_config(name = CONFIG_TYPESAFE_API_KEY, type_ = "password", default = Value::string(""), title = "TypeSafe API Key"),
    string_global_config(name = CONFIG_TYPESAFE_API_BASE, title = "TypeSafe API Base URL", default = DEFAULT_API_BASE),
)]
struct SystemOneModule {
    data: ModuleData,
}

#[async_trait]
impl AsModule for SystemOneModule {
    fn new(ma: ModularAgent, id: String, spec: ModuleSpec) -> Result<Self> {
        Ok(Self {
            data: ModuleData::new(ma, id, spec),
        })
    }

    async fn process(&mut self, ctx: ModuleContext, _port: String, value: Value) -> Result<()> {
        let config = self.configs()?;
        let questions_value = config.get_object(CONFIG_QUESTIONS)?;
        if questions_value.is_empty() {
            return Err(Error::InvalidConfig("questions is empty".to_string()));
        }
        let questions: BTreeMap<String, Question> =
            serde_json::from_value(Value::object(questions_value.clone()).to_json())
                .map_err(|e| Error::InvalidConfig(format!("Invalid questions: {e}")))?;
        let model = config.get_string_or_default(CONFIG_MODEL);

        let response = call_system_one(self.ma(), &value, questions, model).await?;
        self.output(ctx, PORT_ANSWERS, to_value(&response)?).await
    }
}

// ---------------------------------------------------------------------------
// NoulModule
// ---------------------------------------------------------------------------

/// Asks System One a yes/no question and outputs how strongly it holds.
///
/// A "noul" answer is a probability between 0 and 1 that the `instructions`
/// hold for the input. Optional `criteria` describe what each side means, as
/// an object with `true` and/or `false` keys. Empty `criteria` are omitted
/// from the request.
///
/// # Ports
/// - Input `state`: The content to evaluate; string, object, or array
/// - Output `answer`: `{"type": "noul", "noul": <0..1>}`
///
/// # Configuration
/// - `instructions`: The question to answer about the input
/// - `criteria`: Optional `{"true": ..., "false": ...}` descriptions
/// - `model`: Model name (default: "jev-latest")
///
/// # Global Configuration
/// - `typesafe_api_key`: TypeSafe API key; falls back to the `TYPESAFE_API_KEY`
///   environment variable when empty
/// - `typesafe_api_base`: API base URL (default: "https://api.typesafe.ai")
///
/// # Example
/// With `instructions` "Is the sender asking for a refund?" and input
/// `"I'd like my money back for order 1234"`, outputs
/// `{"type": "noul", "noul": 0.95}`.
#[modular_agent(
    title = "Noul",
    category = CATEGORY,
    inputs = [PORT_STATE],
    outputs = [PORT_ANSWER],
    text_config(name = CONFIG_INSTRUCTIONS),
    object_config(name = CONFIG_CRITERIA, detail),
    string_config(name = CONFIG_MODEL, default = DEFAULT_MODEL, detail),
)]
struct NoulModule {
    data: ModuleData,
}

#[async_trait]
impl AsModule for NoulModule {
    fn new(ma: ModularAgent, id: String, spec: ModuleSpec) -> Result<Self> {
        Ok(Self {
            data: ModuleData::new(ma, id, spec),
        })
    }

    async fn process(&mut self, ctx: ModuleContext, _port: String, value: Value) -> Result<()> {
        let config = self.configs()?;
        let instructions = instructions_from(config.get_string_or_default(CONFIG_INSTRUCTIONS));
        let criteria = config.get_object(CONFIG_CRITERIA)?;
        let criteria = if criteria.is_empty() {
            None
        } else {
            Some(Value::object(criteria.clone()).to_json())
        };
        let model = config.get_string_or_default(CONFIG_MODEL);

        let question = Question::Noul {
            instructions,
            criteria,
        };
        let answer = ask_single(self.ma(), &value, question, model).await?;
        self.output(ctx, PORT_ANSWER, answer).await
    }
}

// ---------------------------------------------------------------------------
// ChoiceModule
// ---------------------------------------------------------------------------

/// Asks System One to pick one label from a fixed set.
///
/// `criteria` maps each candidate label to a description (or `null` when the
/// label speaks for itself). The answer names the chosen label along with the
/// model's confidence and the probability of every label. An empty `criteria`
/// is a configuration error.
///
/// # Ports
/// - Input `state`: The content to classify; string, object, or array
/// - Output `answer`: `{"type": "choice", "choice": "<label>", "confidence": <0..1>, "probabilities": {...}}`
///
/// # Configuration
/// - `instructions`: What to decide about the input
/// - `criteria`: `{"<label>": <description or null>, ...}` (required)
/// - `model`: Model name (default: "jev-latest")
///
/// # Global Configuration
/// - `typesafe_api_key`: TypeSafe API key; falls back to the `TYPESAFE_API_KEY`
///   environment variable when empty
/// - `typesafe_api_base`: API base URL (default: "https://api.typesafe.ai")
///
/// # Example
/// With `criteria` `{"billing": null, "bug": "Something is broken", "other": null}`
/// and input `"The app crashes when I open settings"`, outputs
/// `{"type": "choice", "choice": "bug", "confidence": 0.9, "probabilities": {"billing": 0.02, "bug": 0.9, "other": 0.08}}`.
#[modular_agent(
    title = "Choice",
    category = CATEGORY,
    inputs = [PORT_STATE],
    outputs = [PORT_ANSWER],
    text_config(name = CONFIG_INSTRUCTIONS),
    object_config(name = CONFIG_CRITERIA),
    string_config(name = CONFIG_MODEL, default = DEFAULT_MODEL, detail),
)]
struct ChoiceModule {
    data: ModuleData,
}

#[async_trait]
impl AsModule for ChoiceModule {
    fn new(ma: ModularAgent, id: String, spec: ModuleSpec) -> Result<Self> {
        Ok(Self {
            data: ModuleData::new(ma, id, spec),
        })
    }

    async fn process(&mut self, ctx: ModuleContext, _port: String, value: Value) -> Result<()> {
        let config = self.configs()?;
        let instructions = instructions_from(config.get_string_or_default(CONFIG_INSTRUCTIONS));
        let criteria = config.get_object(CONFIG_CRITERIA)?;
        if criteria.is_empty() {
            return Err(Error::InvalidConfig(
                "criteria must list at least one label".to_string(),
            ));
        }
        let criteria = match Value::object(criteria.clone()).to_json() {
            Json::Object(map) => map,
            _ => unreachable!("object config converts to a JSON object"),
        };
        let model = config.get_string_or_default(CONFIG_MODEL);

        let question = Question::Choice {
            instructions,
            criteria,
        };
        let answer = ask_single(self.ma(), &value, question, model).await?;
        self.output(ctx, PORT_ANSWER, answer).await
    }
}

// ---------------------------------------------------------------------------
// ScoreModule
// ---------------------------------------------------------------------------

/// Asks System One to place the input on an ordered scale.
///
/// `criteria` is an ordered array of 2 to 10 entries describing the scale from
/// lowest to highest; entries may be strings or objects. The answer carries the
/// chosen position, the model's confidence, the `legend` echoing the scale,
/// and the probability of every position. Fewer than 2 or more than 10 entries
/// is a configuration error.
///
/// # Ports
/// - Input `state`: The content to rate; string, object, or array
/// - Output `answer`: `{"type": "score", "score": ..., "confidence": <0..1>, "legend": [...], "probabilities": [...]}`
///
/// # Configuration
/// - `instructions`: What aspect of the input to rate
/// - `criteria`: Ordered array of 2 to 10 scale entries (required)
/// - `model`: Model name (default: "jev-latest")
///
/// # Global Configuration
/// - `typesafe_api_key`: TypeSafe API key; falls back to the `TYPESAFE_API_KEY`
///   environment variable when empty
/// - `typesafe_api_base`: API base URL (default: "https://api.typesafe.ai")
///
/// # Example
/// With `instructions` "How urgent is this message?" and `criteria`
/// `["can wait", "this week", "today", "right now"]`, input
/// `"Production is down for all customers"` outputs a score at the top of the
/// scale with its confidence and per-position probabilities.
#[modular_agent(
    title = "Score",
    category = CATEGORY,
    inputs = [PORT_STATE],
    outputs = [PORT_ANSWER],
    text_config(name = CONFIG_INSTRUCTIONS),
    array_config(name = CONFIG_CRITERIA),
    string_config(name = CONFIG_MODEL, default = DEFAULT_MODEL, detail),
)]
struct ScoreModule {
    data: ModuleData,
}

#[async_trait]
impl AsModule for ScoreModule {
    fn new(ma: ModularAgent, id: String, spec: ModuleSpec) -> Result<Self> {
        Ok(Self {
            data: ModuleData::new(ma, id, spec),
        })
    }

    async fn process(&mut self, ctx: ModuleContext, _port: String, value: Value) -> Result<()> {
        let config = self.configs()?;
        let instructions = instructions_from(config.get_string_or_default(CONFIG_INSTRUCTIONS));
        let criteria = config.get_array(CONFIG_CRITERIA)?;
        if !(2..=10).contains(&criteria.len()) {
            return Err(Error::InvalidConfig(format!(
                "criteria must have 2 to 10 entries, got {}",
                criteria.len()
            )));
        }
        let criteria: Vec<Json> = criteria.iter().map(Value::to_json).collect();
        let model = config.get_string_or_default(CONFIG_MODEL);

        let question = Question::Score {
            instructions,
            criteria,
        };
        let answer = ask_single(self.ma(), &value, question, model).await?;
        self.output(ctx, PORT_ANSWER, answer).await
    }
}
