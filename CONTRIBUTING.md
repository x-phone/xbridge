# Contributing to xbridge

Thanks for your interest in contributing!

## Getting Started

```bash
git clone https://github.com/x-phone/xbridge.git
cd xbridge
cargo test      # Run all tests
cargo clippy    # Lint
cargo fmt       # Format
```

## Development Workflow

1. Fork the repo and create a feature branch
2. Make your changes
3. Run `cargo fmt`, `cargo clippy`, and `cargo test`
4. Open a pull request against `main`

## Project Layout

- `src/main.rs` — Entry point, CLI args, server startup
- `src/router.rs` — axum routes, REST handlers, WebSocket handler
- `src/bridge.rs` — SIP bridge, incoming/outbound call wiring
- `src/call_control.rs` — CallControl trait (abstracts xphone::Call for testing)
- `src/state.rs` — Shared application state and registries
- `src/config.rs` — YAML/TOML config loading with env var overrides
- `src/ws.rs` — WebSocket event types (Twilio-compatible + native)
- `src/audio.rs` — PCM/mulaw encoding and conversion
- `src/wav.rs` — Minimal WAV file parser
- `src/webhook.rs` — Webhook event definitions
- `src/webhook_client.rs` — Webhook delivery with retry and dead letter queue
- `src/metrics.rs` — Prometheus metrics
- `src/call.rs` — Call state types
- `src/api.rs` — REST request/response types
- `tests/integration.rs` — Integration tests
- `docs/guide.md` — Integration guide with Python examples

## Code Style

- Run `cargo fmt` and `cargo clippy` before committing
- Keep functions focused and small
- Follow existing patterns in the codebase
- Add tests for new functionality

## Reporting Issues

Open an issue at https://github.com/x-phone/xbridge/issues with:
- What you expected
- What happened instead
- Steps to reproduce
