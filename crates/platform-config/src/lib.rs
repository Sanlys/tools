//! Runtime configuration fetching for egui/wasm frontends.
//!
//! Per-tool backend URLs are deliberately **not** baked into the wasm binary
//! at compile time: the portal backend serves a small `tools.json` (see
//! [`api_types::ToolRegistry`]) that the same wasm build fetches on load, so
//! one build works unmodified across dev/staging/prod and across
//! subdomain-per-tool routing.
//!
//! [`JsonResource`] is the generic building block: it wraps an `ehttp` GET
//! behind a `poll_promise::Promise` so both native and wasm targets can poll
//! it the same way from an `egui::App::update` loop, without an async
//! runtime.

use serde::de::DeserializeOwned;

pub use api_types::{DashboardStatus, ToolLink, ToolRegistry, ToolStatus};

/// A JSON GET request that can be polled from an egui update loop on either
/// native or wasm. Call [`JsonResource::fetch`] once to kick it off, then
/// call [`JsonResource::ready`] every frame until it returns `Some`.
pub struct JsonResource<T: Send + 'static> {
    promise: Option<poll_promise::Promise<Result<T, String>>>,
}

impl<T: Send + 'static> Default for JsonResource<T> {
    fn default() -> Self {
        Self { promise: None }
    }
}

impl<T: DeserializeOwned + Send + 'static> JsonResource<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start (or restart) fetching `url`. Overwrites any in-flight request.
    pub fn fetch(&mut self, url: &str) {
        self.fetch_with_headers(url, &[]);
    }

    /// Like [`fetch`](Self::fetch), but with extra request headers -- e.g. an
    /// `Authorization: Bearer <token>` for a cross-origin authenticated GET
    /// (see `apps/portal/frontend`'s IDP account panel, which calls another
    /// origin's API using its own bearer token rather than a same-origin
    /// runtime-config endpoint).
    pub fn fetch_with_headers(&mut self, url: &str, headers: &[(&str, &str)]) {
        let mut request = ehttp::Request::get(url);
        if !headers.is_empty() {
            request.headers = ehttp::Headers::new(headers);
        }
        let (sender, promise) = poll_promise::Promise::new();
        ehttp::fetch(request, move |response| {
            let result = match response {
                Ok(resp) if resp.ok => serde_json::from_slice::<T>(&resp.bytes)
                    .map_err(|err| format!("decoding {}: {err}", resp.url)),
                Ok(resp) => Err(format!(
                    "{} returned {} {}",
                    resp.url, resp.status, resp.status_text
                )),
                Err(err) => Err(format!("request failed: {err}")),
            };
            sender.send(result);
        });
        self.promise = Some(promise);
    }

    pub fn is_loading(&self) -> bool {
        matches!(&self.promise, Some(p) if p.ready().is_none())
    }

    pub fn has_requested(&self) -> bool {
        self.promise.is_some()
    }

    /// `Some(&Result)` once the request completes; stays `None` while
    /// loading or before [`fetch`](Self::fetch) has been called.
    pub fn ready(&self) -> Option<&Result<T, String>> {
        self.promise.as_ref().and_then(|p| p.ready())
    }
}
