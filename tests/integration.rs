//! A component calls `wasmcloud:html2pdf/converter` through the native
//! `wasmcloud-html2pdf` plugin, and answers an HTTP request with what it got
//! back: the PDF, or the plugin's error for an option out of range.

use anyhow::{Context, Result};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::time::timeout;

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

const CALLER_WASM: &[u8] = include_bytes!("wasm/html2pdf_caller.wasm");

/// Starts a host with the plugin, deploys the caller, and returns the status
/// and body of a request for `path`.
async fn get(path: &str) -> Result<(u16, Vec<u8>)> {
    let engine = Engine::builder().build()?;
    let ingress = Ingress::new(DevRouter::default(), "127.0.0.1:0".parse()?).await?;
    let addr = ingress.addr();

    let host = HostBuilder::new()
        .with_engine(engine)
        .with_http_handler(Arc::new(ingress))
        .with_plugin(Arc::new(Html2Pdf::default()))?
        .build()?
        .start()
        .await
        .context("failed to start host")?;

    let mut http = WitInterface::from("wasi:http/incoming-handler");
    http.config = [("host".to_string(), "test".to_string())]
        .into_iter()
        .collect();

    let started = host
        .workload_start(WorkloadStartRequest {
            workload_id: uuid::Uuid::new_v4().to_string(),
            workload: Workload {
                namespace: "test".to_string(),
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
    anyhow::ensure!(
        started.workload_status.message.contains("started"),
        "workload did not start: {:?}",
        started.workload_status
    );

    let response = timeout(
        Duration::from_secs(60),
        reqwest::Client::new()
            .get(format!("http://{addr}{path}"))
            .header("HOST", "test")
            .send(),
    )
    .await
    .context("request timed out")?
    .context("request failed")?;
    let status = response.status().as_u16();
    let body = response.bytes().await.context("failed to read body")?;
    Ok((status, body.to_vec()))
}

#[tokio::test]
async fn a_component_gets_a_pdf() -> Result<()> {
    let (status, body) = get("/").await?;
    anyhow::ensure!(
        status == 200,
        "status {status}: {}",
        String::from_utf8_lossy(&body)
    );
    anyhow::ensure!(body.starts_with(b"%PDF-"), "not a PDF");
    Ok(())
}

#[tokio::test]
async fn a_component_gets_the_plugins_error() -> Result<()> {
    let (status, body) = get("/bad-options").await?;
    let body = String::from_utf8(body)?;
    anyhow::ensure!(status == 400, "status {status}: {body}");
    anyhow::ensure!(
        body.contains("InvalidOptions") && body.contains("margin"),
        "unexpected error: {body}"
    );
    Ok(())
}
