# douane

douane routes AI requests through subscription accounts. It manages OAuth connections, imported credentials, model availability, quotas and account selection in a Rust service with an embedded Svelte dashboard.

## Build and run

Building the service requires Rust 1.90 or newer and the native linker for your platform. SQLite is bundled. The committed dashboard build is embedded in the executable, so Node.js and pnpm are only needed when changing the frontend.

```sh
cargo build --workspace --release --locked
```

The output contains `douane` and the management CLI `douanectl` under `target/release`, with `.exe` on Windows. You can copy these binaries to another machine with the same operating system and architecture. Runtime data remains in the configured credential directory and SQLite database.

Create `douane.toml`:

```toml
[server]
listen = "127.0.0.1:8787"
auth_dir = "auth"
database = "douane.sqlite"
# client_api_base_url = "http://127.0.0.1:8787/v1"

[settings]
strategy = "smart"
quota_refresh_seconds = 300
request_timeout_seconds = 180

[settings.provider_strategies]
codex = "reset-first"
```

```sh
douane --config douane.toml --check
douane --config douane.toml
```

Relative data paths resolve from the configuration file's directory. An absent configuration file uses the same loopback address, `auth` directory, database and default settings. `--check` validates configuration without creating runtime files. Run the service as the user that owns its credential directory and writable database. Dashboard changes to settings persist in the configuration file.

Open `http://127.0.0.1:8787/admin`. `/management.html` redirects to the dashboard. The default listener serves the local machine; a remote deployment can place authentication in its reverse proxy. Management endpoints also support an optional token through `server.management_token_env`, which names an environment variable. This token does not act as a client API key.

## Connect accounts

The dashboard starts with live empty states. Add accounts through Connecter un compte:

- Claude and Codex use OAuth. Open the authorization link, then paste the returned callback URL or authorization code into the dialog. This also works when the service runs on another machine and the provider's localhost redirect cannot reach it.
- Grok uses device authorization. Enter the displayed code on the provider's page; the dashboard polls until the account is connected or the authorization expires.
- OpenCode Go accepts the subscription bearer credential through the manual form. Its default upstream is `https://opencode.ai/zen/go`.
- JSON import accepts native provider credential objects and supported CLIProxyAPI auth files. Drop files into the dashboard or select them in the connection dialog. Imports report each rejected file.
- Gemini and Antigravity OAuth become available when their runtime OAuth client metadata is configured. Other supported imported account types include Kimi, Qwen and OpenAI-compatible upstreams.

Google OAuth reads `oauth-clients.json` from the parent of `auth_dir`, outside the account credential directory. The object may contain `gemini` and `antigravity` entries with `client_id`, `client_secret` and optional `project_id`. Alternatively set `DOUANE_GEMINI_CLIENT_ID`, `DOUANE_GEMINI_CLIENT_SECRET`, `DOUANE_GEMINI_PROJECT_ID`, or the corresponding `DOUANE_ANTIGRAVITY_*` variables. Environment values take precedence. Restart after changing this startup configuration. The dashboard obtains OAuth availability from the service.

Provider features depend on the account, plan and upstream. An unavailable quota stays unknown; douane does not fabricate an allowance. Credentials remain in local account files. SQLite stores request metadata, status, latency and token/cache counters, without conversation bodies or credential values. Click an account name to rename it. Emails start blurred and can be revealed individually.

## Use a client

Set the client's base URL to `http://127.0.0.1:8787/v1`. Clients that require an API key can use the placeholder `douane`; upstream authentication uses the connected subscription. Choose a model ID returned by `/v1/models`, including its provider prefix.

If the dashboard and client API use different addresses, set `server.client_api_base_url` to the API's HTTP(S) address. The dashboard uses it in client configuration snippets. An origin or a path ending in `/v1` is accepted; an existing proxy path is retained. Omit the setting to use the dashboard's origin. Credentials, query parameters and fragments are rejected.

```sh
curl http://127.0.0.1:8787/v1/models

curl http://127.0.0.1:8787/v1/chat/completions \
  -H 'Authorization: Bearer douane' \
  -H 'Content-Type: application/json' \
  -d '{"model":"<model-id-from-v1-models>","messages":[{"role":"user","content":"Hello"}],"stream":true}'
```

Boite and other OpenAI-compatible clients can use the same base URL, placeholder key and provider-prefixed model ID. Anthropic-compatible clients can call `/v1/messages`.

For Claude Code, set `ANTHROPIC_BASE_URL=http://127.0.0.1:8787` and `ANTHROPIC_AUTH_TOKEN=douane`, then select a connected Claude model. Claude Code adds `/v1/messages` to the base URL. The dashboard's Configurer un outil page provides the client settings.

| Endpoint | Support |
| --- | --- |
| `GET /health` | Service health |
| `GET /v1/models` | Available models |
| `POST /v1/chat/completions` | Chat Completions, including streaming |
| `POST /v1/messages` | Anthropic Messages, including streaming |
| `POST /v1/responses` | Responses, including streaming |
| `POST /v1/responses/compact` | Native compaction on compatible Responses upstreams |
| `GET /v1/responses` with WebSocket upgrade | Native Codex/Responses sessions and a streaming HTTP bridge for other subscriptions |
| `/admin/api/*` | Accounts, quotas, models, request history, settings and OAuth |

HTTP requests can translate between Chat Completions, Messages and Responses. Translation covers the implemented text, tool and image forms; provider-specific extensions are not a promise of full protocol equivalence. Native WebSocket sessions keep one eligible upstream account. Quota and authentication failures make that account temporarily unavailable. A full request can retry on another account before any response event reaches the client. Started output and concurrent requests are never replayed automatically. A continuation that cannot use its original account returns `previous_response_not_found`, allowing the client to reconnect and resend the full conversation.

The HTTP bridge processes queued responses in order, supports cancellation and retains up to eight responses within 64 MiB for incremental conversations. After reconnecting, an unavailable previous response also returns `previous_response_not_found`. Native sockets send periodic heartbeats in both directions, detect silent peers and bound blocked writes.

`/v0/management` and `/v8/management` expose a CLIProxyAPI-compatible subset for auth files, status changes, account models, configuration, usage, logs, models and version. douane does not implement every CLIProxyAPI management operation.

## Quotas and routing

Choose a global strategy in Réglages and optionally override it per provider:

| Value | Behavior for new conversations |
| --- | --- |
| `round-robin` | Rotate through eligible accounts |
| `fill-first` | Keep selecting the first eligible account until it becomes unavailable |
| `reset-first` | Prefer the nearest known weekly reset |
| `smart` | Give 90% of new assignments to the account whose weekly reset arrives first within 48 hours, until exhausted; otherwise weight by remaining quota/time with 20% equal rotation |

Conversation identifiers keep an existing conversation on the same eligible account, which helps preserve upstream cache affinity. Changing a strategy affects new conversations; it does not rebalance active ones. Disabled, exhausted or temporarily unavailable accounts can cause failover to another eligible account of the same provider. Request history reports cache reads and writes when the provider returns them, without estimating prices or promising a latency improvement.

The quota view groups accounts by provider, shows local reset dates, and displays subscription tiers and extra credits reported by the provider. Claude and Codex can expose banked resets. Consuming one requires confirmation in the dashboard. A retry retains the same request and credit, including after a service restart, so a double click cannot consume another reset. Reset availability is read through `GET /admin/api/accounts/{id}/resets`; redemption uses `POST /admin/api/accounts/{id}/resets/redeem` with `confirmation: true` and a UUID `request_id` reused for retries.

For a custom client, send a stable `session_id` header or `prompt_cache_key` for each conversation. Reuse it on every turn and reconnect, and use a different identifier for each independent conversation. Native Codex, Claude and OpenCode session identifiers are recognized; Claude cache controls and native prompt cache keys are preserved. Cache hits still depend on the provider and the reusable prompt prefix.

The affinity table holds up to 10,000 conversations, expires entries after 24 hours without activity, and is kept in memory. A separate bounded table tracks which account produced a native response ID, so response traffic cannot evict conversation bindings. A service restart clears these tables; a WebSocket reconnect to the running service preserves the binding.

## Command line management

```sh
douanectl status
douanectl accounts
douanectl models
douanectl quotas --refresh
douanectl import account.json --name "Subscription account"
douanectl disable <account-id>
douanectl enable <account-id>
douanectl remove <account-id>
douanectl login codex
douanectl complete <oauth-session-id> --callback-file callback.txt
```

Use `--url <service-origin>` to target another listener. `DOUANE_MANAGEMENT_TOKEN` supplies the optional management bearer token for the CLI. Credential import and OAuth completion read files rather than taking secrets as command-line values.

## Development

Frontend changes require Node.js 24 and pnpm 11:

```sh
cd dash
pnpm install --frozen-lockfile
pnpm check
pnpm build
```

Commit the refreshed `dash/build` alongside frontend source changes. Build Rust afterward to embed those assets. `pnpm dev` runs the dashboard development server; it needs the management API at the same origin or a proxy for `/admin/api`.

```sh
cargo test --workspace --locked
```

CI checks Rust on the declared minimum version and validates/builds Svelte. These checks use local test upstreams; they do not prove that a real subscription is authorized or that a provider has enabled a particular model for an account.
