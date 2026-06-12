# lclhst

Share local apps and folders with other devices — over the LAN or peer-to-peer,
no server in the middle, no account, no public URL.

```sh
# share a running app, or a folder as a browsable site
lclhst serve 3000 --name myapp
lclhst serve ./photos
#   ticket: photos@endpoint…                      (for remote peers)
#   on this network:  https://photos.local:4433   (phones, tablets — via mDNS)
#   trust on a phone: http://photos.local:4433/.lclhst/

# another machine, anywhere on the internet
lclhst open myapp@endpoint…
#   https://myapp.localhost:4433 locally, https://myapp.local:4433 for its LAN
```

The tunnel is end-to-end encrypted QUIC via [iroh](https://github.com/n0-computer/iroh)
(direct when hole-punching succeeds; relays see only ciphertext). The serving side
forwards **only** the target you put on the command line — the protocol has no port
field, so the other side can't ask for anything else. Concurrent requests, websockets,
and dev-server HMR work; QUIC multiplexes streams natively. LAN exposure is the
default (`--local-only` opts out): names travel as `<name>.local` over mDNS, and the
edge answers plain http with a redirect to https.

Certificates are minted from a per-machine CA (`~/.config/lclhst`). Run
`lclhst trust` once for a clean padlock on this machine; phones open
`http://<name>.local:4433/.lclhst/` to download and trust the CA — or skip it and
click through the one-time warning (traffic is encrypted either way).

macOS note: answering mDNS needs the **Local Network** permission (System Settings →
Privacy & Security) for your terminal; without it the `.local` name won't resolve,
but the printed by-IP URL always works.

Inspired by [localias](https://github.com/peterldowns/localias) and
[sendme](https://github.com/n0-computer/sendme). See `docs/commits.md` for the
commit-message convention.

## Install

```sh
cargo install lclhst
```

## License

MIT or Apache-2.0, at your option.
