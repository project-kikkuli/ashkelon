# ashkelon

A local gateway for coding agents. Point an agent's model traffic at it and it:

- relays every call to the real provider byte for byte (thinking signatures and encrypted reasoning survive);
- logs one JSON line per call: model, tokens (reasoning included, exact or estimated), time to first byte, total time, stop reason;
- applies built-in request transforms and rules, the only code allowed in the request path;
- runs your hooks in the background on agent events, and reports a failing hook back to the agent as a ping. Nothing ever blocks the agent.

It never reads, stores, or refreshes an agent's login. It forwards the auth headers the agent itself sent.

## Install

```sh
cargo install --path .
```

## Run

```sh
ashkelon run claude                      # Claude Code
ashkelon run codex                       # Codex on a ChatGPT login
ashkelon run codex -- --openai-api       # Codex on an OpenAI API key
ashkelon run opencode                    # opencode (TUI via serve + attach)
ashkelon run opencode -- run "prompt"    # opencode headless
ashkelon run omp
ashkelon run hermes
ashkelon run ori
```

Anything after `--` goes to the agent. `run` starts a relay on a free local port, starts the agent pointed at it through environment variables or temporary config files, and exits with the agent's exit code. Your real config files are never modified: omp and Hermes run against a temporary home that links back to every file in the real one, with only the model settings replaced, and anything they create is moved back on exit.

For an agent you configure yourself, run the relay on its own and point the agent's base URL at it:

```sh
ashkelon serve                           # 127.0.0.1:8484 unless `listen` is set
```

| Route | Upstream |
| --- | --- |
| `/anthropic/...` | `https://api.anthropic.com` |
| `/openai/...` | `https://api.openai.com` |
| `/chatgpt/...` | `https://chatgpt.com` (Codex on a ChatGPT login) |
| `/openrouter/...` | `https://openrouter.ai` |
| `/opencode/...` | `https://opencode.ai` (OpenCode Zen) |
| `/ollama/...` | `http://127.0.0.1:11434` |
| `/<name>/...` | any `[[routes]]` entry in the config |

`run` prefixes routes with `/s/<launch-id>/` so calls can be tied to the launch that made them.

### What `run` refuses

- Hermes on its built-in Anthropic provider: Hermes reads and refreshes Claude Code's stored login, which logs out every running Claude Code session. Use an API-key provider (OpenRouter, Anthropic with a key).
- Cursor: it has no way to change where it sends model requests.

## Configure

`~/.config/ashkelon/config.toml` (or `--config <path>`). Every section is optional; [`examples/config.toml`](examples/config.toml) shows all of them.

| Section | What it does |
| --- | --- |
| `listen`, `log_dir`, `state_dir`, `log_bodies` | Where things go. `log_bodies = true` also keeps request and response bodies. |
| `[[routes]]` | Extra upstreams: `name`, `upstream`. |
| `[transforms.tool_output]` | Shorten tool output over `max_chars`, keeping `keep_head` and `keep_tail` characters. Deterministic, so prompt caching still hits. |
| `[[transforms.strip]]` | Remove regex matches from user text. |
| `[rules]` | `allow_models` (regexes), `max_output_tokens`: reject a request before it is sent. `max_response_chars`, `[[rules.cut_patterns]]`: cut a streaming response; the agent gets the provider's own error shape. |
| `[[hooks]]` | Background hooks (below). |
| `[pings]` | `max_per_session`, `wake_idle`, `idle_after_secs`, `max_concurrent_hooks`. |
| `[[models]]` | Models hooks may call with `ashkelon model`: `name`, `api` (`anthropic`, `openai`, `openai_chat`), `base_url`, `model`, `api_key_env`. |

## Hooks

A hook is a command that runs in the background when an event fires:

| Event | When |
| --- | --- |
| `session_start` | First request of a session |
| `prompt` | A request ending in a new user message |
| `tool_call` | A response asks for tools |
| `tool_result` | A request carries tool results |
| `turn_end` | A response finishes without asking for a tool |
| `compaction` | The conversation got shorter (compacted or rewound) |

```toml
[[hooks]]
name = "tests-with-changes"
on = ["turn_end"]
command = ["~/.config/ashkelon/hooks/tests-with-changes.sh"]
projects = ["~/src/myproject"]     # optional: only sessions working under these paths
harnesses = ["claude", "codex"]    # optional
timeout_secs = 120
```

The hook gets the event as JSON on stdin: `event`, `hook`, `harness`, `session`, `launch`, `cwd`, `state_dir`, `request_path` (the latest request body), `response_path` (the latest assistant text), `ts`, plus `prompt`, `tool_calls`, or `text` when the event has them. Environment: `ASHKELON_EVENT`, `ASHKELON_SESSION_DIR`, `ASHKELON_BIN`.

It prints one JSON object:

```json
{"status": "fail", "message": "what is wrong", "fix": "what to do about it"}
```

`pass` clears any earlier failure of that hook for the session. A `fail` becomes a ping:

```
<ashkelon-ping hook="tests-with-changes" id="…">
what is wrong
fix: what to do about it
</ashkelon-ping>
```

The same failure is never pinged twice, and each session has a cap. A hook can use a model through `$ASHKELON_BIN model <name> --system "…"` (prompt on stdin, text on stdout); see [`examples/hooks/`](examples/hooks/).

### How a ping reaches the agent

- Agent working: the ping is added to its next request, pinned at the same position in every later request so the cached prompt prefix stays stable.
- Agent idle: ashkelon wakes it. Claude Code through an MCP channel server that `run` adds (`--no-channel` to skip), Codex through `codex queue`, opencode through its server API, anything else by typing into its tmux pane when launched inside tmux.

## Call log

`<log_dir>/calls-YYYY-MM-DD.jsonl` (UTC dates), one object per call. Message content is never included.

| Field | |
| --- | --- |
| `ts`, `call_id` | When, and a unique id |
| `session` | `{launch, harness, session}`; `session` is the agent's own session id when it sends one |
| `route`, `wire`, `method`, `path`, `status` | `wire` is `anthropic_messages`, `openai_responses`, `openai_chat`, or `opaque` |
| `model`, `stop_reason`, `turn_end`, `tool_calls` | Parsed from the response |
| `usage` | `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`, `reasoning_tokens`, `reasoning_estimated` |
| `ttfb_ms`, `total_ms`, `request_bytes`, `response_bytes` | Timing and size |
| `transforms`, `pings_injected`, `rule`, `error` | What ashkelon did to the call |

Hook runs are logged to `<log_dir>/hooks-YYYY-MM-DD.jsonl`. The default `log_dir` is `~/Library/Application Support/ashkelon/logs` on macOS and `$XDG_STATE_HOME/ashkelon/logs` elsewhere.

## Develop

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

Tests run against local fake providers with dummy keys; they never call a real one.
