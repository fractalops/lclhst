# lclhst

Share a running localhost app with one other person, peer-to-peer.
No server in the middle, no account, no public URL — the ticket is the capability.

```sh
# machine A — has the app
lclhst serve 3000 --name myapp
# ticket: myapp@nodeac4f…

# machine B — wants to see it
lclhst open myapp@nodeac4f…
# serving myapp → https://myapp.localhost:4433
```

The connection is end-to-end encrypted QUIC via [iroh](https://github.com/n0-computer/iroh)
(direct when hole-punching succeeds; relays see only ciphertext). The serving side
forwards **only** the port you put on the command line — the protocol has no port
field, so the other side can't ask for anything else. Concurrent requests, websockets,
and dev-server HMR work; QUIC multiplexes streams natively.

v0.1 uses a self-signed certificate: browsers warn (click through), `curl -k` is clean.
A local CA with trust installation is the v0.2 headline.

Inspired by [localias](https://github.com/peterldowns/localias) and
[sendme](https://github.com/n0-computer/sendme). See `SPEC.md` for the design and
`docs/commits.md` for the commit-message convention.

## Install

```sh
cargo install lclhst
```

## License

MIT or Apache-2.0, at your option.
