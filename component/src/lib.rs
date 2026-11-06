//! Calls `wasmcloud:html2pdf/converter` from an HTTP request. `/` answers with
//! the PDF of a small document; `/bad-options` asks for a margin too large for
//! the page and answers with the error the plugin returned.

mod bindings {
    wit_bindgen::generate!({
        world: "component",
        generate_all
    });
}

use bindings::{
    exports::wasi::http::incoming_handler::Guest,
    wasi::http::types::{
        Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
    },
    wasmcloud::html2pdf::converter::{self, Options},
};

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        // Two 150 mm margins do not fit on an A4 page.
        let margin = match request.path_with_query().as_deref() {
            Some("/bad-options") => Some(150.0),
            _ => None,
        };
        let options = Options {
            page_width: None,
            page_height: None,
            margin,
            page_numbers: None,
            header: None,
            footer: None,
        };
        let (status, payload) =
            match converter::convert("<h1>Hello from a component</h1>", &options) {
                Ok(pdf) => (200, pdf),
                Err(err) => (400, format!("{err:?}").into_bytes()),
            };

        let response = OutgoingResponse::new(Fields::new());
        response.set_status_code(status).unwrap();
        let body = response.body().unwrap();
        ResponseOutparam::set(response_out, Ok(response));

        let stream = body.write().unwrap();
        // `blocking-write-and-flush` takes at most 4096 bytes per call.
        for chunk in payload.chunks(4096) {
            stream.blocking_write_and_flush(chunk).unwrap();
        }
        drop(stream);
        OutgoingBody::finish(body, None).unwrap();
    }
}

bindings::export!(Component with_types_in bindings);
