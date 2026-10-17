# corvo-chat-server

The server for corvo-chat, end-to-end encrypted 1:1 chat. It stores and
forwards ciphertext only. It gives out 10-digit numbers, holds messages
while the other person is offline, and lets new accounts in by invite.

**Status:** early development.

## Development

```bash
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

## License

MIT. See [LICENSE](LICENSE).
