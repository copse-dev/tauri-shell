# tauri-shell

A [Tauri](https://tauri.app/) shell rendered by [Servo](https://servo.org/)
via [`tauri-runtime-servo`](https://github.com/copse-dev/tauri-runtime-servo),
which owns OS windows and nothing else. The application it hosts runs as a
Node **sidecar** this process spawns; the frontend it renders is read off disk
at runtime.

Neither is compiled in, and that is the whole point. One published binary
serves any app that speaks the protocol below — so the engine is built once,
here, on a public repository's free runner minutes, and consumed everywhere
else as a download rather than a 20-minute compile.

## Using a release

```bash
COPSE_FRONTEND_DIR=path/to/dist/renderer \
COPSE_SIDECAR_ENTRY=path/to/dist/sidecar/index.js \
  ./tauri-shell
```

| variable | default | meaning |
| -------- | ------- | ------- |
| `COPSE_FRONTEND_DIR` | `dist/renderer` | directory served at `copse://localhost/` |
| `COPSE_SIDECAR_ENTRY` | `../dist/sidecar/index.js`, then exe-relative | the sidecar's entry point |
| `COPSE_SIDECAR_NODE` | `node` | the node binary to spawn it with |

The sidecar is spawned with `COPSE_TAURI_SHELL=1` in its environment, so it
can tell it is running under this shell rather than under Electron.

## The protocol

Line-oriented JSON over the sidecar's stdio. Every protocol line is prefixed
`@copse-tauri `; anything else on stdout is passed through as sidecar logging,
so a sidecar can print freely without corrupting the channel.

**Sidecar → shell**

```jsonc
{"op":"create-window","winId":1,"url":"index.html?winId=1",
 "width":1200,"height":800,"minWidth":800,"minHeight":600,
 "title":"App","show":false,"backgroundColor":"#1e1e1e"}

{"op":"window","winId":1,"action":"show"}
// show | hide | focus | close | maximize | minimize
```

`url` is resolved against `copse://localhost/`, so it is whatever path the
frontend directory holds, plus any query the sidecar wants the page to see.

**Shell → sidecar**

```jsonc
{"op":"window-event","winId":1,"event":"focus"}
// close-requested | closed | focus | blur
```

The shell knows nothing beyond this. How the renderer reaches the sidecar —
a loopback WebSocket, in the case this was extracted from — is the sidecar's
business, arranged through the query string it puts in `url`.

The shell exits when the sidecar's stdout closes. Windows closing does not end
it: an app may legitimately have none for a while.

## Why the frontend is served, not embedded

`tauri_build` normally bakes `frontendDist` into the executable, which would
tie every build to one application. Instead the shell registers a `copse://`
URI scheme and serves the directory `COPSE_FRONTEND_DIR` names.

That is not merely equivalent to the embedded path, it is equivalent *in the
ways that matter*: on the patched engine a registered custom scheme gets a
tuple origin and counts as potentially trustworthy, so the page keeps CSP
`'self'` and the secure-context APIs it would have had on `tauri://localhost`.
Verified rather than assumed — a page served this way under an enforced
`default-src 'self'` loads its own subresources on the patched engine, and is
blocked on a stock one.

Path traversal is refused rather than normalised: a request naming `..` is a
403 in the log, not a read of an arbitrary file.

## The engine

`Cargo.toml` pins the `tauri-runtime-patches` branches of the org's engine
forks by rev, and `patched-servo` is on by default. That block is the one
[`tauri-runtime-servo`](https://github.com/copse-dev/tauri-runtime-servo)
publishes under "Using a patched Servo"; its CI builds that recipe on every
change, so keep the two in step rather than editing this copy alone.

Enabling `patched-servo` against a stock libservo is a compile error rather
than a silent no-op, so the feature and the overrides cannot drift apart
unnoticed. To build against stock published libservo instead:

```bash
cargo build --release --no-default-features
```

Expect the first build to take the better part of an hour: it compiles Servo.

## Licence

Apache-2.0 OR MIT.
