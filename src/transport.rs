use core::future::Future;
use std::sync::{Arc, Mutex};

use js_sys::{Promise, Uint8Array};
use monero_daemon_rpc::HttpTransport;
use monero_wallet::interface::InterfaceError;
use rand_core::{OsRng, RngCore as _};
use send_wrapper::SendWrapper;
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, Response};

use crate::auth::{Challenge, Credentials, authorization, parse_challenge, request_path};

#[wasm_bindgen]
extern "C" {
  // The global `fetch`, present in windows, workers, service workers and Node 18+.
  #[wasm_bindgen(catch, js_name = fetch)]
  fn global_fetch(input: &Request) -> Result<Promise, JsValue>;
}

#[derive(Default)]
struct AuthState {
  challenge: Option<Challenge>,
  nonce_count: u32,
}

/// `HttpTransport` backed by the JS `fetch` API, with optional HTTP Digest/Basic login.
#[derive(Clone)]
pub struct FetchTransport {
  base_url: String,
  credentials: Option<Credentials>,
  // Reusing the last challenge saves a 401 round trip per request.
  auth: Arc<Mutex<AuthState>>,
}

impl FetchTransport {
  pub fn new(base_url: &str, credentials: Option<Credentials>) -> Self {
    Self {
      base_url: base_url.trim_end_matches('/').to_owned(),
      credentials,
      auth: Arc::default(),
    }
  }
}

fn js_error(context: &str, value: &JsValue) -> InterfaceError {
  InterfaceError::InterfaceError(format!("{context}: {value:?}"))
}

fn cnonce() -> String {
  let mut bytes = [0u8; 16];
  OsRng.fill_bytes(&mut bytes);
  hex::encode(bytes)
}

async fn fetch(url: &str, body: &[u8], authorization: Option<&str>) -> Result<Response, InterfaceError> {
  let init = RequestInit::new();
  init.set_method("POST");
  init.set_body(&Uint8Array::from(body).into());
  let request =
    Request::new_with_str_and_init(url, &init).map_err(|e| js_error("building request", &e))?;
  if let Some(authorization) = authorization {
    request
      .headers()
      .set("Authorization", authorization)
      .map_err(|e| js_error("setting Authorization header", &e))?;
  }
  let promise = global_fetch(&request).map_err(|e| js_error("fetch", &e))?;
  JsFuture::from(promise)
    .await
    .map_err(|e| js_error("fetch", &e))?
    .dyn_into()
    .map_err(|e| js_error("fetch returned a non-Response", &e))
}

impl FetchTransport {
  fn next_authorization(&self, url: &str) -> Option<String> {
    let credentials = self.credentials.as_ref()?;
    let mut state = self.auth.lock().ok()?;
    let challenge = state.challenge.clone()?;
    state.nonce_count += 1;
    Some(
      authorization(credentials, &challenge, "POST", request_path(url), state.nonce_count, &cnonce())
        .to_string(),
    )
  }

  fn store_challenge(&self, challenge: Option<Challenge>) {
    if let Ok(mut state) = self.auth.lock() {
      *state = AuthState { challenge, nonce_count: 0 };
    }
  }

  async fn post(
    self,
    url: String,
    body: Vec<u8>,
    response_size_limit: Option<usize>,
  ) -> Result<Vec<u8>, InterfaceError> {
    let cached_authorization = self.next_authorization(&url);
    let mut response = fetch(&url, &body, cached_authorization.as_deref()).await?;

    if response.status() == 401 {
      if self.credentials.is_none() {
        return Err(InterfaceError::InterfaceError(format!(
          "{url} requires a login (set the daemon username and password)"
        )));
      }
      let header = response
        .headers()
        .get("www-authenticate")
        .map_err(|e| js_error("reading WWW-Authenticate", &e))?
        .unwrap_or_default();
      let challenge = parse_challenge(&header).ok_or_else(|| {
        InterfaceError::InterfaceError(format!("unsupported login challenge from {url}: {header}"))
      })?;
      // monerod only honours a nonce on the connection that issued it; draining the body frees that
      // keep-alive connection so fetch reuses it for the retry.
      if let Ok(drained) = response.array_buffer() {
        let _ = JsFuture::from(drained).await;
      }
      self.store_challenge(Some(challenge));
      let authorization = self.next_authorization(&url);
      response = fetch(&url, &body, authorization.as_deref()).await?;
      if response.status() == 401 {
        self.store_challenge(None);
        return Err(InterfaceError::InterfaceError(format!(
          "login rejected by {url} (check the daemon username and password)"
        )));
      }
    }

    if !response.ok() {
      return Err(InterfaceError::InterfaceError(format!(
        "HTTP {} from {url}",
        response.status()
      )));
    }

    let buffer = JsFuture::from(response.array_buffer().map_err(|e| js_error("reading body", &e))?)
      .await
      .map_err(|e| js_error("reading body", &e))?;
    let bytes = Uint8Array::new(&buffer).to_vec();
    if response_size_limit.is_some_and(|limit| bytes.len() > limit) {
      return Err(InterfaceError::InterfaceError(format!(
        "response from {url} exceeded size limit"
      )));
    }
    Ok(bytes)
  }
}

impl HttpTransport for FetchTransport {
  fn post(
    &self,
    route: &str,
    body: Vec<u8>,
    response_size_limit: Option<usize>,
  ) -> impl Send + Future<Output = Result<Vec<u8>, InterfaceError>> {
    let url = format!("{}/{route}", self.base_url);
    // wasm32-unknown-unknown is single-threaded; SendWrapper panics if that ever stops being true.
    SendWrapper::new(self.clone().post(url, body, response_size_limit))
  }
}
