//! # wasmcloud:html2pdf Plugin
//!
//! This module renders HTML into PDF for components, in Rust with `printpdf`.
//! It implements the `wasmcloud:html2pdf/converter` interface.
//!
//! The layout runs inside the host process, with no browser and no network.
//! The document loads nothing: images, external stylesheets and web fonts are
//! left out, and scripts do not run. Text uses the fonts installed on the
//! host, which the plugin finds on the first call and keeps for later ones.
//!
//! Layout is CPU work, so each conversion runs on a blocking thread. At most
//! [`MAX_CONCURRENT`] run at once, and further calls wait for a free one.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use printpdf::html::{SharedFontPool, build_font_pool};
use printpdf::{Base64OrRaw, GeneratePdfOptions, PdfDocument, PdfSaveOptions};
use tokio::sync::Semaphore;
use tracing::instrument;

use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wit::{WitInterface, WitWorld};

pub(crate) const PLUGIN_HTML2PDF_ID: &str = "wasmcloud-html2pdf";

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "html2pdf",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wasmcloud::html2pdf::converter::{ConvertError, Options};

/// The default page: A4, in millimeters.
const A4_WIDTH: f32 = 210.0;
const A4_HEIGHT: f32 = 297.0;

/// How many layouts may run at once. Further calls wait for a free one.
const MAX_CONCURRENT: usize = 4;

/// How long a component waits for one conversion.
const TIMEOUT: Duration = Duration::from_secs(30);

pub struct Html2Pdf {
    /// One permit for each layout that runs now.
    layouts: Arc<Semaphore>,
    /// The fonts installed on the host, found on the first call.
    fonts: Arc<OnceLock<SharedFontPool>>,
}

impl Default for Html2Pdf {
    fn default() -> Self {
        Self {
            layouts: Arc::new(Semaphore::new(MAX_CONCURRENT)),
            fonts: Arc::new(OnceLock::new()),
        }
    }
}

impl<'a> bindings::wasmcloud::html2pdf::converter::Host for ActiveCtx<'a> {
    #[instrument(name = "wasmcloud.html2pdf.convert", skip(self, html, options))]
    async fn convert(
        &mut self,
        html: String,
        options: Options,
    ) -> wash_runtime::wasmtime::Result<Result<Vec<u8>, ConvertError>> {
        let plugin = self.try_get_plugin::<Html2Pdf>(PLUGIN_HTML2PDF_ID)?;
        Ok(plugin.convert(html, &options).await)
    }
}

impl Html2Pdf {
    async fn convert(&self, html: String, options: &Options) -> Result<Vec<u8>, ConvertError> {
        let options = generate_options(options)?;
        let permit = self
            .layouts
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ConvertError::Failed("the layout limiter is closed".to_string()))?;
        let fonts = self.fonts.clone();
        // The permit moves into the thread, so a layout that outlives the
        // timeout still counts against the limit until it ends.
        let layout = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            render(&html, &options, &fonts)
        });
        match tokio::time::timeout(TIMEOUT, layout).await {
            Ok(Ok(pdf)) => pdf,
            // A panic in the layout engine fails this call, not the host.
            Ok(Err(err)) => Err(ConvertError::Failed(format!("the layout stopped: {err}"))),
            Err(_) => Err(ConvertError::Timeout),
        }
    }
}

/// Checks `options` and turns them into `printpdf` settings.
fn generate_options(options: &Options) -> Result<GeneratePdfOptions, ConvertError> {
    let invalid = |reason: String| Err(ConvertError::InvalidOptions(reason));
    let width = options.page_width.unwrap_or(A4_WIDTH);
    let height = options.page_height.unwrap_or(A4_HEIGHT);
    for (name, value) in [("page-width", width), ("page-height", height)] {
        if !(value.is_finite() && value > 0.0) {
            return invalid(format!("`{name}` must be a number above 0, not {value}"));
        }
    }
    let margin = options.margin.unwrap_or(0.0);
    if !(margin.is_finite() && margin >= 0.0) {
        return invalid(format!("`margin` must be 0 or more, not {margin}"));
    }
    if 2.0 * margin >= width.min(height) {
        return invalid(format!(
            "`margin` {margin} leaves no room on a {width} x {height} mm page"
        ));
    }

    Ok(GeneratePdfOptions {
        page_width: Some(width),
        page_height: Some(height),
        margin_top: Some(margin),
        margin_right: Some(margin),
        margin_bottom: Some(margin),
        margin_left: Some(margin),
        show_page_numbers: options.page_numbers,
        header_text: options.header.clone(),
        footer_text: options.footer.clone(),
        ..GeneratePdfOptions::default()
    })
}

/// Lays out `html` and writes the PDF. Runs on a blocking thread.
fn render(
    html: &str,
    options: &GeneratePdfOptions,
    fonts: &OnceLock<SharedFontPool>,
) -> Result<Vec<u8>, ConvertError> {
    // Finding the host's fonts reads every font directory, so do it once.
    let fonts = fonts
        .get_or_init(|| build_font_pool(&BTreeMap::new(), None))
        .clone();
    // No images and no extra fonts: the document gets no files.
    let no_files: BTreeMap<String, Base64OrRaw> = BTreeMap::new();
    let mut warnings = Vec::new();
    let document = PdfDocument::from_html_with_cache(
        html,
        &no_files,
        &no_files,
        options,
        &mut warnings,
        Some(fonts),
    )
    .map_err(ConvertError::Failed)?;

    // A layout that fails gives a document with no pages, and puts the reason
    // only in `warnings`.
    if document.pages.is_empty() {
        let reason = warnings
            .iter()
            .map(|warning| warning.msg.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(ConvertError::Failed(if reason.is_empty() {
            "the document has no pages".to_string()
        } else {
            reason
        }));
    }
    Ok(document.save(&PdfSaveOptions::default(), &mut warnings))
}

#[async_trait::async_trait]
impl HostPlugin for Html2Pdf {
    fn id(&self) -> &'static str {
        PLUGIN_HTML2PDF_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wasmcloud:html2pdf/converter")]),
            ..Default::default()
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        component_handle: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wasmcloud", "html2pdf", &[]) {
            tracing::warn!(
                "Html2Pdf plugin requested for non-wasmcloud:html2pdf interface(s): {:?}",
                interfaces
            );
            return Ok(());
        }

        // Add `wasmcloud:html2pdf/converter` to the workload's linker
        bindings::wasmcloud::html2pdf::converter::add_to_linker::<_, SharedCtx>(
            component_handle.linker()?,
            extract_active_ctx,
        )?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            page_width: None,
            page_height: None,
            margin: None,
            page_numbers: None,
            header: None,
            footer: None,
        }
    }

    #[test]
    fn empty_options_take_an_a4_page() {
        let params = generate_options(&options()).unwrap();
        assert_eq!(params.page_width, Some(210.0));
        assert_eq!(params.page_height, Some(297.0));
        for margin in [
            params.margin_top,
            params.margin_right,
            params.margin_bottom,
            params.margin_left,
        ] {
            assert_eq!(margin, Some(0.0));
        }
        assert_eq!(params.show_page_numbers, None);
        assert_eq!(params.header_text, None);
        assert_eq!(params.footer_text, None);
    }

    #[test]
    fn options_become_generate_options() {
        let params = generate_options(&Options {
            page_width: Some(215.9),
            page_height: Some(279.4),
            margin: Some(12.5),
            page_numbers: Some(true),
            header: Some("Invoice".to_string()),
            footer: Some("Thank you".to_string()),
        })
        .unwrap();
        assert_eq!(params.page_width, Some(215.9));
        assert_eq!(params.page_height, Some(279.4));
        for margin in [
            params.margin_top,
            params.margin_right,
            params.margin_bottom,
            params.margin_left,
        ] {
            assert_eq!(margin, Some(12.5));
        }
        assert_eq!(params.show_page_numbers, Some(true));
        assert_eq!(params.header_text.as_deref(), Some("Invoice"));
        assert_eq!(params.footer_text.as_deref(), Some("Thank you"));
    }

    #[test]
    fn rejects_out_of_range_options() {
        let with = |set: fn(&mut Options)| {
            let mut options = options();
            set(&mut options);
            options
        };
        let bad = [
            ("page-width", with(|o| o.page_width = Some(0.0))),
            ("page-width", with(|o| o.page_width = Some(f32::NAN))),
            ("page-height", with(|o| o.page_height = Some(-1.0))),
            ("page-height", with(|o| o.page_height = Some(f32::INFINITY))),
            ("margin", with(|o| o.margin = Some(-0.1))),
            ("margin", with(|o| o.margin = Some(f32::NAN))),
            // Two margins of 105 mm cover the whole width of an A4 page.
            ("margin", with(|o| o.margin = Some(105.0))),
        ];
        for (name, options) in bad {
            match generate_options(&options) {
                Err(ConvertError::InvalidOptions(reason)) => {
                    assert!(reason.contains(name), "{name}: {reason}")
                }
                other => panic!("{name}: expected invalid-options, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn converts_html_to_pdf() {
        let pdf = Html2Pdf::default()
            .convert(
                "<h1>Hello, wasmCloud!</h1><p>A PDF made in Rust.</p>".to_string(),
                &options(),
            )
            .await
            .unwrap();
        assert!(pdf.starts_with(b"%PDF-"), "not a PDF");
    }

    #[tokio::test]
    async fn the_document_cannot_reach_the_network() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let html = format!(
            r#"<link rel="stylesheet" href="{url}/style.css">
               <img src="{url}/image.png">
               <iframe src="{url}/frame.html"></iframe>
               <iframe src="file:///etc/hostname"></iframe>
               <p>blocked</p>"#
        );

        let pdf = Html2Pdf::default().convert(html, &options()).await.unwrap();
        assert!(pdf.starts_with(b"%PDF-"), "not a PDF");
        assert!(
            tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_err(),
            "the document reached the network"
        );
    }

    #[tokio::test]
    async fn bad_options_fail_before_the_layout_starts() {
        let mut options = options();
        options.margin = Some(500.0);
        assert!(matches!(
            Html2Pdf::default().convert(String::new(), &options).await,
            Err(ConvertError::InvalidOptions(_))
        ));
    }
}
