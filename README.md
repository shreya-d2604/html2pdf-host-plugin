# html2pdf-host-plugin

A host plugin for [wasmCloud](https://github.com/wasmCloud/wasmCloud)'s
`wash-runtime` that lets WebAssembly components turn HTML into PDF. The host
does the layout itself, in pure Rust with [`printpdf`](https://crates.io/crates/printpdf):
no headless browser, no network calls, no extra service to run.

> Community plugin for wasmCloud, not an official wasmCloud project.

```
component ──(wasmcloud:html2pdf/converter.convert)──▶ host plugin ──▶ printpdf ──▶ PDF bytes
```

## The interface

Components import one function:

```wit
package wasmcloud:html2pdf@0.1.0;

interface converter {
  record options {
    page-width: option<f32>,    // mm, default 210 (A4)
    page-height: option<f32>,   // mm, default 297 (A4)
    margin: option<f32>,        // mm, all four sides, default 0
    page-numbers: option<bool>, // "Page X of Y" at the bottom
    header: option<string>,
    footer: option<string>,
  }

  variant convert-error {
    invalid-options(string),
    timeout,
    failed(string),
  }

  convert: func(html: string, options: options) -> result<list<u8>, convert-error>;
}
```

The full definition, with docs, is in
[`wit/deps/wasmcloud-html2pdf-0.1.0/package.wit`](wit/deps/wasmcloud-html2pdf-0.1.0/package.wit).

## How the plugin behaves

- **Sandboxed input.** The document loads nothing by URL: images, external
  stylesheets and web fonts are left out, and scripts do not run. Put styles
  in a `<style>` element.
- **Off the async runtime.** Layout is CPU work, so each conversion runs on a
  blocking thread. At most 4 run at once; further calls wait their turn.
- **Bounded.** A conversion that takes longer than 30 s returns `timeout`. A
  panic in the layout engine fails that one call, not the host.
- **Host fonts.** Text uses the fonts installed on the host, found on the
  first call and reused after that.
- **Checked options.** Sizes and margins out of range come back as
  `invalid-options` with the reason, e.g. a margin that leaves no room on the
  page.

## Calling it from a component

```rust
use bindings::wasmcloud::html2pdf::converter::{self, Options};

let options = Options {
    page_width: None,
    page_height: None,
    margin: Some(15.0),
    page_numbers: Some(true),
    header: None,
    footer: None,
};
let pdf: Vec<u8> = converter::convert("<h1>Hello from a component</h1>", &options)?;
```

[`component/`](component/) has a complete example: an HTTP component that
answers `GET /` with a PDF, and `GET /bad-options` with the plugin's error.

## Try it

You need Rust 1.95+ with the `wasm32-wasip2` target (`rust-toolchain.toml`
pins both).

```sh
# integration tests: start a host with the plugin, deploy the component, request a PDF
cargo test

# or run it as a server
cargo run --example server
curl -H 'Host: demo' http://127.0.0.1:8080/ -o hello.pdf
```

[`examples/hello.pdf`](examples/hello.pdf) is what you get back.

## Using it in your own host

Register the plugin on the host, and list the interface in the workload's
`host_interfaces`:

```rust
use html2pdf_host_plugin::Html2Pdf;

let host = HostBuilder::new()
    .with_engine(engine)
    .with_http_handler(Arc::new(ingress))
    .with_plugin(Arc::new(Html2Pdf::default()))?
    .build()?
    .start()
    .await?;

// in the WorkloadStartRequest:
host_interfaces: vec![
    http,
    WitInterface::from("wasmcloud:html2pdf/converter@0.1.0"),
],
```

[`examples/server.rs`](examples/server.rs) is the whole thing in about 60
lines.

## Layout

```
src/lib.rs            the host plugin (HostPlugin impl + printpdf layout)
wit/                  the wasmcloud:html2pdf WIT package
component/            a guest component that calls the plugin
tests/integration.rs  host + plugin + component, end to end
tests/wasm/           the built component the tests deploy
examples/server.rs    the same, as a server on port 8080
```

To rebuild the component after changing it:

```sh
(cd component && cargo build --target wasm32-wasip2 --release)
cp component/target/wasm32-wasip2/release/html2pdf_caller.wasm tests/wasm/
```

## Compatibility

Built against `wash-runtime` from wasmCloud
[`v2.12.0`](https://github.com/wasmCloud/wasmCloud/releases/tag/v2.12.0),
pulled in as a git dependency (`wash-runtime` is not published on crates.io).

## License

Apache-2.0, the same as wasmCloud.
