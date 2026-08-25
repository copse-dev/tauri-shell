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
TAURI_SHELL_FRONTEND_DIR=path/to/dist/renderer \
TAURI_SHELL_SIDECAR_ENTRY=path/to/dist/sidecar/index.js \
  ./tauri-shell
```

| variable | default | meaning |
| -------- | ------- | ------- |
| `TAURI_SHELL_FRONTEND_DIR` | `dist/renderer` | directory served at `<scheme>://localhost/` |
| `TAURI_SHELL_SIDECAR_ENTRY` | `../dist/sidecar/index.js`, then exe-relative | the sidecar's entry point |
| `TAURI_SHELL_SIDECAR_NODE` | `node` | the node binary to spawn it with |
| `TAURI_SHELL_SCHEME` | `app` | the scheme the frontend is served under |
| `TAURI_SHELL_APP_NAME` | `Tauri Shell` | tauri's default menu and Linux desktop integration |

The sidecar is spawned with `TAURI_SHELL=1` in its environment, so it can tell
it is running under this shell rather than under Electron.

### Naming the application on macOS

`TAURI_SHELL_APP_NAME` sets the name tauri uses for its own default menu and,
on Linux, for desktop integration. It does **not** reach the macOS application
menu — the one at the top left, beside the Apple logo — and it is worth being
explicit about why, because it looks as though it should.

`tauri_build` embeds an Info.plist in the executable's `__TEXT,__info_plist`
section with `CFBundleName` taken from this crate's `productName`, and AppKit
reads that for the application menu before any of this code runs. So a bare
shell binary calls itself "Tauri Shell" whatever its windows are titled, and no
runtime setting can reach it: mutating tauri's package info changes a different
menu, and renaming the executable loses to the embedded section.

The fix is a bundle, and it belongs to the host rather than here. Launched from
inside an `.app`, that bundle's Info.plist wins:

```
YourApp.app/
  Contents/
    Info.plist          # CFBundleName, CFBundleIdentifier, your icon
    MacOS/tauri-shell   # this binary, unmodified
```

Give it your own `CFBundleIdentifier` rather than reusing another app's: macOS
keys permission grants and saved window state off it. The wrapper is yours, the
executable inside it stays generic — which is this whole arrangement in
miniature.

The scheme is configurable because it *is* the page's origin: it decides what
CSP `'self'` resolves to and what origin-scoped storage is keyed by, so an
application that wants its own should have it. Anything that is not a legal
scheme is refused with a log line rather than quietly producing a URL that
resolves somewhere unexpected.

## The protocol

Line-oriented JSON over the sidecar's stdio. Every protocol line is prefixed
`@tauri-shell `; anything else on stdout is passed through as sidecar logging,
so a sidecar can print freely without corrupting the channel.

**Sidecar → shell**

```jsonc
{"op":"create-window","winId":1,"url":"index.html?winId=1",
 "width":1200,"height":800,"minWidth":800,"minHeight":600,
 "title":"App","show":false,"backgroundColor":"#1e1e1e"}

{"op":"window","winId":1,"action":"show"}
// show | hide | focus | close | maximize | minimize
```

`url` is resolved against `<scheme>://localhost/`, so it is whatever path the
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

Both ends of this are easy to get half-right, and the failure is silent — a
sidecar that does not see `TAURI_SHELL=1`, or does not use the prefix above,
simply never asks for a window, and you get a process that starts and shows
nothing. So the shell says so if no `create-window` arrives within fifteen
seconds.

## Why the frontend is served, not embedded

`tauri_build` normally bakes `frontendDist` into the executable, which would
tie every build to one application. Instead the shell registers a URI scheme
at startup and serves the directory `TAURI_SHELL_FRONTEND_DIR` names.

That is not merely equivalent to the embedded path, it is equivalent *in the
ways that matter*: on the patched engine a registered custom scheme gets a
tuple origin and counts as potentially trustworthy, so the page keeps CSP
`'self'` and the secure-context APIs it would have had on `tauri://localhost`.
Verified rather than assumed, and on schemes invented for the test rather than
on `tauri://` itself: a page served this way under an enforced
`default-src 'self'` loads its own subresources on the patched engine, and is
blocked on a stock one. The fix is a property of embedder-registered schemes
as a class, which is what makes an arbitrary `TAURI_SHELL_SCHEME` safe.

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
