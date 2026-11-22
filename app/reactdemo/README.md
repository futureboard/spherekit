# reactdemo — SphereKit Studio

React 19 running inside SphereKit's own V8 isolate, styled by the shared CSS runtime, rendered
natively through Direct3D 11 — or Skia on the CPU when there is no GPU. No browser, no WebView, no
Node.

```text
cargo run -p reactdemo --release
SPHEREKIT_BACKEND=skia cargo run -p reactdemo --release      # the CPU fallback
cargo run -p reactdemo --release -- --screenshot out.png --page team [--scale 2]
```

`--screenshot` renders one page with no window — the whole React → V8 → bridge → layout →
renderer path, read back from an offscreen surface — and exits. Pages: `mixer`, `tasks`, `team`,
`about`.

## The app

| Page | What it exercises |
|---|---|
| **Mixer** | Faders, pan knobs, mute/solo, vertical level meters. The levels are synthesised in Rust and raised as a `meters` event every frame — but only while the page has asked for it with `app.setAnimating`, so leaving the tab stops the redraw loop. |
| **Tasks** | A controlled `TextField` (every keystroke round-trips through React state), Enter to submit, checkboxes, filters, derived counts, `:nth-child(even)` row striping from the stylesheet. |
| **Team** | A searchable roster in Latin, Thai, Japanese, Korean and Greek — shaping and font fallback, with every glyph rasterised by DirectWrite on Windows. |
| **About** | `useInvoke("app.info")`: the backend (D3D11 or Skia), adapter, V8 and React versions, as the host reports them. |

The header's fps / CPU / frame readout is driven by a `frame` event the host raises on its own, and
native code can switch pages with a `navigate` event.

## The bridge, both ways

| Direction | Name | Payload |
|---|---|---|
| JS → native | `app.info` | → `{ backend, adapter, v8, renderer, scale, initMs }` |
| JS → native | `app.setAnimating` | `{ on }` |
| JS → native | `app.quit` | — |
| native → JS | `frame` | `{ count, fps, cpuMs }` |
| native → JS | `meters` | `{ levels: number[4] }` |
| native → JS | `navigate` | `{ page }` |

Both sides are named once: `renderer/src/native.ts` in TypeScript, `start_javascript` and `draw` in
`src/main.rs`.

## The build

`build.rs` bundles `renderer/src/main.tsx` with Bun into a single IIFE script and writes it to
`OUT_DIR`, where `include_str!` picks it up. The binary carries its renderer and never depends on a
`dist/` directory surviving on disk.

Install the workspace's JavaScript dependencies once, from the repository root (it is a Bun
workspace so that React is a single instance across the renderer and `@spherekit/react`):

```text
bun install
cargo build -p reactdemo
```

**Bun is not required.** Without it the build falls back to a renderer written directly against the
API Bridge wire protocol, with no React and no bundler. It still runs and still styles itself from
the same stylesheet, and is worth reading precisely because it shows what React's reconciler
eventually produces: one `commit` frame.

V8 itself is a prebuilt static library that the `v8` crate (rusty_v8) downloads on the first
build and links straight into the executable; there is no DLL or ICU file to ship. See
`docs/javascript.md`.

## The stylesheet

`styles/app.css` is installed once into the shared `spherekit-css` runtime. `ReactHost` resolves
every committed React node through it. The status bar at the bottom of the window is a native
`div()` that no React component knows exists, and it is resolved through the very same rules. One
cascade, two producers, no browser.

## Platform

If V8 fails to start, the app still runs, rendering with Skia, and shows a native panel
explaining that the JavaScript runtime is unavailable.
