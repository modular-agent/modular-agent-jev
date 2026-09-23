# TypeSafe AI System One (Jev) modules for Modular Agent

Modules that ask [TypeSafe AI](https://typesafe.ai)'s System One model (Jev) structured
questions about a piece of input: a yes/no probability, a choice among labels, or a
position on an ordered scale. No prompt engineering, no free-form text to parse.

## Features

- **System One** — Ask several questions about one input in a single request and
  receive the full API response
- **Noul** — Ask one yes/no question and receive how strongly it holds (0 to 1)
- **Choice** — Pick one label from a fixed set, with confidence and per-label
  probabilities
- **Score** — Place the input on an ordered scale of 2 to 10 entries

All modules take the value to evaluate on the `state` input port. Strings, objects, and
arrays are sent to the API as-is.

## Usage

This crate builds as part of the
[modular-agent monorepo](https://github.com/modular-agent/modular-agent). Clone it into
the monorepo's `custom_modules/` directory and select it with the ma-config wizard:

```sh
cd modular-agent/custom_modules
git clone https://github.com/modular-agent/modular-agent-jev.git
cd ..
cargo run --manifest-path tools/ma-config/Cargo.toml -- desktop   # or: cli
```

## Example

`examples/showcase.json` wires one customer message into all four modules, extracts
the chosen label with Get Value, formats the score with Template String, and shows the
System One error port. Copy it into `~/.modular_agent/patches/` and open it in the
desktop app.

## Setup

### Global Config or Environment Variables

| Global Config       | Environment Variable | Description                                       |
| ------------------- | -------------------- | ------------------------------------------------- |
| `typesafe_api_key`  | `TYPESAFE_API_KEY`   | TypeSafe API key                                  |
| `typesafe_api_base` | -                    | API base URL (default: `https://api.typesafe.ai`) |

Set them under **Settings → Module → System One** in the desktop app. The API key
falls back to the environment variable when the global config is empty.

## System One

Sends the input and a map of named questions in one request. Use it when one input
needs several judgements at once; the single-question modules below cover the common
case without hand-written JSON.

### Configuration

| Config      | Type   | Default        | Description |
| ----------- | ------ | -------------- | ----------- |
| `questions` | object | `{}`           | -           |
| `model`     | string | `"jev-latest"` | -           |

Each entry in `questions` is keyed by a name of your choosing:

```json
{
  "spam": { "type": "noul", "instructions": "Is this spam?" },
  "tone": {
    "type": "choice",
    "instructions": "What is the tone?",
    "criteria": { "positive": null, "negative": "Complaints or anger" }
  },
  "urgency": {
    "type": "score",
    "instructions": "How urgent is this?",
    "criteria": ["can wait", "this week", "today"]
  }
}
```

### Ports

- **Input**: `state` — The content to evaluate; string, object, or array
- **Output**: `answers` — The full response: `model`, `answers` (keyed like `questions`,
  each carrying a `type` field), and `usage` (`input_tokens`, `output_tokens`)

## Noul

### Configuration

| Config         | Type   | Default        | Description |
| -------------- | ------ | -------------- | ----------- |
| `instructions` | text   | `""`           | -           |
| `criteria`     | object | `{}`           | -           |
| `model`        | string | `"jev-latest"` | -           |

`criteria` optionally describes each side as `{"true": ..., "false": ...}`; an empty
object is omitted from the request.

### Ports

- **Input**: `state` — The content to evaluate
- **Output**: `answer` — `{"type": "noul", "noul": <0..1>}`

## Choice

### Configuration

| Config         | Type   | Default        | Description |
| -------------- | ------ | -------------- | ----------- |
| `instructions` | text   | `""`           | -           |
| `criteria`     | object | `{}`           | -           |
| `model`        | string | `"jev-latest"` | -           |

`criteria` maps each candidate label to a description, or `null` when the label speaks
for itself. It must list at least one label.

### Ports

- **Input**: `state` — The content to classify
- **Output**: `answer` —
  `{"type": "choice", "choice": "<label>", "confidence": <0..1>, "probabilities": {...}}`

## Score

### Configuration

| Config         | Type   | Default        | Description |
| -------------- | ------ | -------------- | ----------- |
| `instructions` | text   | `""`           | -           |
| `criteria`     | array  | `[]`           | -           |
| `model`        | string | `"jev-latest"` | -           |

`criteria` is an ordered array of 2 to 10 entries describing the scale from lowest to
highest; entries may be strings or objects.

### Ports

- **Input**: `state` — The content to rate
- **Output**: `answer` —
  `{"type": "score", "score": ..., "confidence": <0..1>, "legend": [...], "probabilities": [...]}`

## Error Handling

Rate limits (HTTP 429) and overloads (HTTP 529) are retried up to three times with
exponential backoff, honouring `Retry-After`. Connection failures and timeouts are
retried the same way. A rejected API key (HTTP 401) surfaces as a configuration error
and an invalid request (HTTP 422) as a value error carrying the API's message.

## Environment Variables

| Variable           | Purpose                                                          |
| ------------------ | ---------------------------------------------------------------- |
| `TYPESAFE_API_KEY` | API key, used when the `typesafe_api_key` global config is empty |

## Architecture

The HTTP client (`src/client.rs`) is independent of the module layer and holds the
request/response types and the retry loop. Modules share one client per
`(base URL, API key)` pair. Network tests against the live API are ignored by default:

```sh
TYPESAFE_API_KEY=... cargo test -p modular-agent-jev -- --ignored
```

## Key Dependencies

- [reqwest](https://crates.io/crates/reqwest) — HTTP client (rustls)

## License

Licensed under either of [Apache License, Version 2.0](LICENSE_APACHE-2.0) or
[MIT license](LICENSE_MIT) at your option.
