# CLAUDE.md — modular-agent-jev

## Overview

Modules for TypeSafe AI's System One API (model family "Jev"): one generic
`SystemOneModule` plus single-question `Noul` / `Choice` / `Score` modules.

## Layout

- `src/client.rs` — serde types for the request/response schema and
  `TypeSafeClient` (HTTP, retry with backoff, status-to-`Error` mapping).
  No dependency on the module layer, so it is unit-testable and reusable.
- `src/modules.rs` — the four modules, global config lookup with the
  `TYPESAFE_API_KEY` env fallback, and a client cache keyed by
  `(base URL, API key)`.
- `tests/system_one.rs` — `#[ignore]` network test against the live API.

## Conventions

- Global configs (`typesafe_api_key`, `typesafe_api_base`) are declared on
  `SystemOneModule` only; the other modules read them through
  `SystemOneModule::DEF_NAME`.
- Single-question modules send `questions = {"answer": ...}` and emit
  `answers.answer` on the `answer` port as the whole Answer object. No
  dedicated ports for fields like `choice`; extract with std key-path modules.
- `instructions` is always sent (as `null` when empty); `criteria` on `noul`
  is omitted when empty.
