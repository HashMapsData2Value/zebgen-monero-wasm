use core::future::Future;
use std::sync::{Arc, Mutex};

use js_sys::{Promise, Reflect, Uint8Array};
use monero_daemon_rpc::HttpTransport;
use monero_wallet::interface::InterfaceError;
use rand_core::{OsRng, RngCore as _};
use send_wrapper::SendWrapper;
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortController, AbortSignal, ReadableStreamDefaultReader, Request, RequestInit, Response};

use crate::auth::{Challenge, Credentials, authorization, parse_challenge, request_path};

#[wasm_bindgen]
extern "C" {
  // The global `fetch`, present in windows, workers, service workers and Node 18+.
  #[wasm_bindgen(catch, js_name = fetch)]
  fn global_fetch(input: &Request) -> Result<Promise, JsValue>;

  #[wasm_bindgen(catch, js_name = setTimeout)]
  fn set_timeout(callback: &js_sys::Function, milliseconds: i32) -> Result<JsValue, JsValue>;

  #[wasm_bindgen(js_name = clearTimeout)]
  fn clear_timeout(handle: &JsValue);
}

const REQUEST_TIMEOUT_MS: i32 = 60_000;

struct RequestDeadline {
  controller: AbortController,
  handle: JsValue,
  _callback: Closure<dyn FnMut()>,
  external: Option<(AbortSignal, Closure<dyn FnMut()>)>,
  complete: bool,
}

impl RequestDeadline {
  fn new(signal: Option<&AbortSignal>) -> Result<Self, InterfaceError> {
    let controller =
      AbortController::new().map_err(|e| js_error("creating abort controller", &e))?;
    let abort = controller.clone();
    let callback = Closure::wrap(Box::new(move || abort.abort()) as Box<dyn FnMut()>);
    let handle = set_timeout(callback.as_ref().unchecked_ref(), REQUEST_TIMEOUT_MS)
      .map_err(|e| js_error("starting request timeout", &e))?;
    let mut deadline = Self {
      controller,
      handle,
      _callback: callback,
      external: None,
      complete: false,
    };
    if let Some(signal) = signal {
      let abort = deadline.controller.clone();
      let listener = Closure::wrap(Box::new(move || abort.abort()) as Box<dyn FnMut()>);
      signal.add_event_listener_with_callback("abort", listener.as_ref().unchecked_ref())
        .map_err(|e| js_error("linking abort signal", &e))?;
      deadline.external = Some((signal.clone(), listener));
      if signal.aborted() {
        deadline.controller.abort();
      }
    }
    Ok(deadline)
  }
}

impl Drop for RequestDeadline {
  fn drop(&mut self) {
    clear_timeout(&self.handle);
    if let Some((signal, listener)) = &self.external {
      let _ = signal.remove_event_listener_with_callback("abort", listener.as_ref().unchecked_ref());
    }
    if !self.complete {
      self.controller.abort();
    }
  }
}

struct BodyReader {
  reader: ReadableStreamDefaultReader,
  complete: bool,
}

impl Drop for BodyReader {
  fn drop(&mut self) {
    if !self.complete {
      let cancellation = self.reader.cancel();
      wasm_bindgen_futures::spawn_local(async move {
        let _ = JsFuture::from(cancellation).await;
      });
    }
    self.reader.release_lock();
  }
}

async fn read_body(
  response: &Response,
  url: &str,
  limit: Option<usize>,
  retain: bool,
) -> Result<Vec<u8>, InterfaceError> {
  let Some(stream) = response.body() else {
    return Ok(Vec::new());
  };
  let reader = stream
    .get_reader()
    .dyn_into::<ReadableStreamDefaultReader>()
    .map_err(|e| js_error("creating body reader", &e))?;
  let mut reader = BodyReader {
    reader,
    complete: false,
  };
  let mut bytes = Vec::new();
  let mut total = 0usize;
  loop {
    let result = JsFuture::from(reader.reader.read())
      .await
      .map_err(|e| js_error("reading body", &e))?;
    let done = Reflect::get(&result, &JsValue::from_str("done"))
      .map_err(|e| js_error("reading body completion", &e))?;
    if done.as_bool() == Some(true) {
      reader.complete = true;
      return Ok(bytes);
    }
    let chunk = Reflect::get(&result, &JsValue::from_str("value"))
      .map_err(|e| js_error("reading body chunk", &e))?
      .dyn_into::<Uint8Array>()
      .map_err(|e| js_error("body chunk was not bytes", &e))?;
    total = total
      .checked_add(chunk.length() as usize)
      .filter(|total| limit.is_none_or(|limit| *total <= limit))
      .ok_or_else(|| {
        InterfaceError::InterfaceError(format!("response from {url} exceeded size limit"))
      })?;
    if retain {
      let start = bytes.len();
      bytes.resize(total, 0);
      chunk.copy_to(&mut bytes[start..]);
    }
  }
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
  signal: Option<AbortSignal>,
}

impl FetchTransport {
  pub fn new(base_url: &str, credentials: Option<Credentials>) -> Self {
    Self {
      base_url: base_url.trim_end_matches('/').to_owned(),
      credentials,
      auth: Arc::default(),
      signal: None,
    }
  }

  pub fn with_signal(mut self, signal: Option<AbortSignal>) -> Self {
    self.signal = signal;
    self
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

async fn fetch(
  url: &str,
  body: &[u8],
  authorization: Option<&str>,
  deadline: &RequestDeadline,
) -> Result<Response, InterfaceError> {
  let init = RequestInit::new();
  init.set_method("POST");
  init.set_signal(Some(&deadline.controller.signal()));
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
    let mut deadline = RequestDeadline::new(self.signal.as_ref())?;
    let cached_authorization = self.next_authorization(&url);
    let mut response = fetch(&url, &body, cached_authorization.as_deref(), &deadline).await?;

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
      read_body(&response, &url, response_size_limit, false).await?;
      self.store_challenge(Some(challenge));
      let authorization = self.next_authorization(&url);
      response = fetch(&url, &body, authorization.as_deref(), &deadline).await?;
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

    let bytes = read_body(&response, &url, response_size_limit, true).await?;
    deadline.complete = true;
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
