# Browser worker integration

Build the Rust Wasm output, then arrange for `pkg/marksheet_wasm.js` to be
served adjacent to this directory. With `wasm-bindgen` installed, the direct
development command is:

```sh
cargo build --manifest-path bindings/wasm/Cargo.toml --target wasm32-unknown-unknown --release
wasm-bindgen --target web --out-dir bindings/wasm/pkg \
  bindings/wasm/target/wasm32-unknown-unknown/release/marksheet_wasm.wasm
```

The site imports a client instead of calling Wasm directly:

```ts
import { MarksheetWorkerClient } from "../../bindings/wasm/web/client.js";

const client = new MarksheetWorkerClient(
  () => new Worker(new URL("../../bindings/wasm/web/worker.js", import.meta.url), { type: "module" }),
);
await client.open(new Uint8Array(await file.arrayBuffer()));
```

Use `await client.cancelAndRestart()` for cancellation. It terminates the old
worker and reopens exactly the last response-accepted source, so no in-flight
edit can be mistaken for a committed document. Before persisting a local file,
call `saveWithExternalChangeGuard`; `external_drift` is a rebase handoff, not a
write permission. Its byte-for-byte guard rejects same-length changes too.

Run the dependency-free proof tests with `npm test` from this directory.
