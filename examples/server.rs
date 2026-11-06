//! Runs a host with the html2pdf plugin and the caller component on port 8080.
//!
//!     cargo run --example server
//!     curl -H 'Host: demo' http://127.0.0.1:8080/ -o hello.pdf

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result};
use html2pdf_host_plugin::Html2Pdf;
use wash_runtime::{
    engine::Engine,
    host::{
        HostApi, HostBuilder,
        http::{DevRouter, Ingress},
    },
    types::{Component, Workload, WorkloadStartRequest},
    wit::WitInterface,
};

const CALLER_WASM: &[u8] = include_bytes!("../tests/wasm/html2pdf_caller.wasm");

#[tokio::main]
async fn main() -> Result<()> {
    let ingress = Ingress::new(DevRouter::default(), "127.0.0.1:8080".parse()?).await?;
    let host = HostBuilder::new()
        .with_engine(Engine::builder().build()?)
        .with_http_handler(Arc::new(ingress))
        .with_plugin(Arc::new(Html2Pdf::default()))?
        .build()?
        .start()
        .await
        .context("failed to start host")?;

    let mut http = WitInterface::from("wasi:http/incoming-handler");
    http.config = [("host".to_string(), "demo".to_string())]
        .into_iter()
        .collect();

    host.workload_start(WorkloadStartRequest {
        workload_id: uuid::Uuid::new_v4().to_string(),
        workload: Workload {
            namespace: "demo".to_string(),
            name: "html2pdf".to_string(),
            annotations: HashMap::new(),
            service: None,
            components: vec![Component {
                name: "caller".to_string(),
                bytes: bytes::Bytes::from_static(CALLER_WASM),
                ..Default::default()
            }],
            host_interfaces: vec![
                http,
                WitInterface::from("wasmcloud:html2pdf/converter@0.1.0"),
            ],
            volumes: vec![],
        },
    })
    .await
    .context("failed to start workload")?;

    println!("listening on http://127.0.0.1:8080 (Ctrl-C to stop)");
    println!("try: curl -H 'Host: demo' http://127.0.0.1:8080/ -o hello.pdf");
    tokio::signal::ctrl_c().await?;
    Ok(())
}
