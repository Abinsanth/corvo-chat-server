# corvo-chat-server

The server for corvo-chat, end-to-end encrypted 1:1 chat. It stores and
forwards ciphertext only. Each account gets a username and a 10-digit
number, joins by invite, and signs in with OPAQUE (the server never sees the
password) from up to 3 devices. People start chats with a request, keep the
full encrypted history on the server, and every device stays in sync, with
delivered and read marks and online status for contacts.

**Status:** early development. It runs at `https://chat.corvo.live`.

- To run your own, see [docs/deploy.md](docs/deploy.md).
- To write a client, see [docs/protocol.md](docs/protocol.md).

## Development

```bash
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

## License

MIT. See [LICENSE](LICENSE).
