//! Transport-independent CDP state for the portable browser.
//!
//! This module deliberately does not depend on Tokio, sockets, threads, or a
//! WebSocket implementation. A host owns the transport and calls the small
//! JSON ABI below. Commands which need host services (JavaScript evaluation,
//! navigation I/O, screenshot, and PDF encoding) are represented as bounded,
//! opaque actions. The host completes an action after doing the platform work;
//! response and event ordering remain owned here.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use url::Url;
use wasm_bindgen::prelude::*;

use obscura_cdp::action::{ActionQueue, HostActionResult};
use obscura_cdp::engine::{
    ActionId as EngineActionId, ActionResult as EngineActionResult, CdpEngine, CdpFailure,
    ContextId, ContextOptions, EngineAction, PageId,
};
use obscura_cdp::protocol::{MAX_MESSAGE_BYTES, MAX_METHOD_BYTES, MAX_SESSION_BYTES};
use obscura_cdp::state::{BrowserState, ConnectionId, SessionId};

use crate::ObscuraCore;
use crate::navigation::{MAX_NAVIGATION_HEADERS_BYTES, MAX_NAVIGATION_URL_BYTES};

pub const CDP_ABI_VERSION: u32 = 1;
const MAX_EVENT_QUEUE: usize = 512;
const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ACTION_RESULT_BYTES: usize = 16 * 1024 * 1024;
const MAX_STREAM_BYTES: usize = 12 * 1024 * 1024;
const MAX_STREAM_CHUNK_BYTES: usize = 1 * 1024 * 1024;
const MAX_RESPONSE_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_BODIES: usize = 128;
const MAX_FETCH_PATTERNS: usize = 64;
const MAX_FETCH_PATTERN_BYTES: usize = 2048;
const MAX_FETCH_REQUESTS: usize = 256;
const MAX_FETCH_RESOLUTIONS: usize = 256;

fn js_error(message: &str) -> JsValue {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Error::new(message).into()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = message;
        JsValue::NULL
    }
}

fn js_range_error(message: &str) -> JsValue {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::RangeError::new(message).into()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = message;
        JsValue::NULL
    }
}

fn bounded(value: &str, maximum: usize, label: &str) -> Result<(), JsValue> {
    if value.len() > maximum {
        return Err(js_range_error(&format!(
            "{label} exceeds the {maximum}-byte limit"
        )));
    }
    Ok(())
}

fn fetch_url_matches(pattern: &str, url: &str) -> bool {
    if pattern == "*" || pattern.is_empty() {
        return pattern == "*";
    }
    let pattern: Vec<char> = pattern.chars().collect();
    let url: Vec<char> = url.chars().collect();
    let mut pattern_index = 0;
    let mut url_index = 0;
    let mut star_index = None;
    let mut star_match = 0;
    while url_index < url.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == url[url_index])
        {
            pattern_index += 1;
            url_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star_index = Some(pattern_index);
            star_match = url_index;
            pattern_index += 1;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            star_match += 1;
            url_index = star_match;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == '*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

fn error_value(code: i32, message: impl Into<String>) -> Value {
    json!({"code": code, "message": message.into()})
}

fn cdp_error_response(id: &Value, code: i32, message: impl Into<String>, session: Option<&str>) -> Value {
    let mut response = Map::new();
    response.insert("id".to_string(), id.clone());
    response.insert("error".to_string(), error_value(code, message));
    if let Some(session) = session {
        response.insert("sessionId".to_string(), Value::String(session.to_string()));
    }
    Value::Object(response)
}

fn cdp_error_response_with_data(
    id: &Value,
    code: i32,
    message: impl Into<String>,
    data: Option<&Value>,
    session: Option<&str>,
) -> Value {
    let mut response = cdp_error_response(id, code, message, session);
    if let Some(data) = data {
        if let Some(error) = response.get_mut("error").and_then(Value::as_object_mut) {
            error.insert("data".to_string(), data.clone());
        }
    }
    response
}

fn cdp_result_response(id: &Value, result: Value, session: Option<&str>) -> Value {
    let mut response = Map::new();
    response.insert("id".to_string(), id.clone());
    response.insert("result".to_string(), result);
    if let Some(session) = session {
        response.insert("sessionId".to_string(), Value::String(session.to_string()));
    }
    Value::Object(response)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    id: Value,
    method: String,
    #[serde(default)]
    params: Map<String, Value>,
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Clone)]
struct Action {
    connection_id: u32,
    request_id: Value,
    session_id: Option<String>,
    target_id: String,
    kind: String,
}

struct ResponseBody {
    body: String,
    base64_encoded: bool,
}

struct PausedFetch {
    request_id: String,
    url: String,
    method: String,
    headers: BTreeMap<String, String>,
    post_data: Option<String>,
    resource_type: String,
    frame_id: String,
    request_stage: String,
}

#[derive(Debug, Clone)]
struct FetchPattern {
    url_pattern: String,
    request_stage: String,
}

#[derive(Clone, Copy)]
struct Viewport {
    width: u32,
    height: u32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self { width: 800, height: 600 }
    }
}

struct Target {
    id: String,
    context_id: String,
    frame_id: String,
    loader_id: String,
    url: String,
    title: String,
    document_handle: u32,
    revision: u32,
    viewport: Viewport,
    emulated_media: String,
    focus_emulation: bool,
    cache_disabled: bool,
    extra_headers: BTreeMap<String, String>,
    network_enabled_sessions: BTreeSet<String>,
    fetch_patterns: BTreeMap<String, Vec<FetchPattern>>,
    paused_fetches: BTreeMap<String, PausedFetch>,
    response_bodies: BTreeMap<String, ResponseBody>,
    core: ObscuraCore,
    sessions: BTreeSet<String>,
}

struct Connection {
    browser_session: String,
    sessions: HashMap<String, String>,
    events: VecDeque<Value>,
    discover_targets: bool,
    auto_attach: bool,
}

/// Portable CDP state and command dispatcher.
///
/// A single instance can own multiple connections and targets. It is safe to
/// keep it in one WASM Worker and expose it to any host transport. No socket or
/// native runtime is reachable from this type.
#[wasm_bindgen]
pub struct PortableCdp {
    /// Shared transport-free identity state. The legacy maps below retain the
    /// string-shaped CDP payloads during migration, while this state owns
    /// monotonic context/page/connection/session lifetimes for the eventual
    /// shared dispatcher cutover.
    shared_state: BrowserState,
    shared_contexts: BTreeMap<String, ContextId>,
    shared_pages: BTreeMap<String, PageId>,
    shared_connections: BTreeMap<u32, ConnectionId>,
    shared_sessions: BTreeMap<String, SessionId>,
    shared_actions: BTreeMap<u32, EngineActionId>,
    targets: BTreeMap<String, Target>,
    connections: BTreeMap<u32, Connection>,
    actions: BTreeMap<u32, Action>,
    action_queue: ActionQueue,
    contexts: BTreeSet<String>,
    next_connection_id: u32,
    next_target_id: u32,
    next_session_id: u32,
    next_loader_id: u32,
    streams: obscura_cdp::io::IoStreamStore,
    stream_connections: BTreeMap<String, u32>,
    fetch_resolutions: VecDeque<Value>,
}

#[wasm_bindgen]
impl PortableCdp {
    #[wasm_bindgen(constructor)]
    pub fn new(html: &str) -> Result<Self, JsValue> {
        let core = ObscuraCore::new(html)?;
        let document_handle = core.document_handle();
        let revision = core.page_revision();
        let mut shared_state = BrowserState::new();
        let default_context = shared_state.default_context();
        let default_page = shared_state
            .create_page(&default_context, "about:blank")
            .map_err(|error| js_error(&format!("shared CDP state initialization failed: {error}")))?;
        let mut targets = BTreeMap::new();
        targets.insert(
            "page-1".to_string(),
            Target {
                id: "page-1".to_string(),
                context_id: "default".to_string(),
                frame_id: "page-1".to_string(),
                loader_id: "loader-blank-page-1".to_string(),
                url: "about:blank".to_string(),
                title: String::new(),
                document_handle,
                revision,
                viewport: Viewport::default(),
                emulated_media: String::new(),
                focus_emulation: false,
                cache_disabled: false,
                extra_headers: BTreeMap::new(),
                network_enabled_sessions: BTreeSet::new(),
                fetch_patterns: BTreeMap::new(),
                paused_fetches: BTreeMap::new(),
                response_bodies: BTreeMap::new(),
                core,
                sessions: BTreeSet::new(),
            },
        );
        let mut contexts = BTreeSet::new();
        contexts.insert("default".to_string());
        Ok(Self {
            shared_state,
            shared_contexts: BTreeMap::from([("default".to_string(), default_context)]),
            shared_pages: BTreeMap::from([("page-1".to_string(), default_page)]),
            shared_connections: BTreeMap::new(),
            shared_sessions: BTreeMap::new(),
            shared_actions: BTreeMap::new(),
            targets,
            connections: BTreeMap::new(),
            actions: BTreeMap::new(),
            action_queue: ActionQueue::new(1),
            contexts,
            next_connection_id: 0,
            next_target_id: 1,
            next_session_id: 0,
            next_loader_id: 0,
            streams: obscura_cdp::io::IoStreamStore::with_limits(128, MAX_STREAM_BYTES),
            stream_connections: BTreeMap::new(),
            fetch_resolutions: VecDeque::new(),
        })
    }

    /// Copy one bounded host-produced base64 payload into a WASM-owned stream.
    /// The returned opaque handle is consumed by IO.read/IO.close through the
    /// same portable CDP command envelope.
    #[wasm_bindgen(js_name = openStream)]
    pub fn open_stream(&mut self, connection_id: u32, data_base64: &str) -> Result<String, JsValue> {
        if !self.connections.contains_key(&connection_id) {
            return Err(js_error("unknown CDP connection"));
        }
        bounded(data_base64, MAX_ACTION_RESULT_BYTES, "CDP stream data")?;
        let data = BASE64
            .decode(data_base64)
            .map_err(|_| js_error("CDP stream data is not valid base64"))?;
        if data.len() > MAX_STREAM_BYTES {
            return Err(js_range_error(&format!(
                "CDP stream data exceeds the {MAX_STREAM_BYTES}-byte limit"
            )));
        }
        let handle = self
            .streams
            .insert(data)
            .map_err(|error| js_error(&error))?;
        self.stream_connections.insert(handle.clone(), connection_id);
        Ok(handle)
    }

    /// Register one host connection and return its opaque ID.
    #[wasm_bindgen(js_name = openConnection)]
    pub fn open_connection(&mut self) -> Result<u32, JsValue> {
        let id = self
            .next_connection_id
            .checked_add(1)
            .ok_or_else(|| js_range_error("CDP connection ID space is exhausted"))?;
        let shared_id = self
            .shared_state
            .open_connection()
            .map_err(|error| js_error(&format!("shared CDP connection allocation failed: {error}")))?;
        self.next_connection_id = id;
        self.shared_connections.insert(id, shared_id);
        self.connections.insert(
            id,
            Connection {
                browser_session: format!("browser-connection-{id}"),
                sessions: HashMap::new(),
                events: VecDeque::new(),
                discover_targets: false,
                auto_attach: false,
            },
        );
        Ok(id)
    }

    /// Close a connection and release all target/session/action ownership.
    #[wasm_bindgen(js_name = closeConnection)]
    pub fn close_connection(&mut self, connection_id: u32) -> Result<(), JsValue> {
        let Some(connection) = self.connections.remove(&connection_id) else {
            return Ok(());
        };
        let session_ids: BTreeSet<String> = connection.sessions.keys().cloned().collect();
        for session_id in session_ids {
            if let Some(target_id) = connection.sessions.get(&session_id) {
                if let Some(target) = self.targets.get_mut(target_id) {
                    target.sessions.remove(&session_id);
                    target.network_enabled_sessions.remove(&session_id);
                    target.fetch_patterns.remove(&session_id);
                    if target.fetch_patterns.is_empty() {
                        target.paused_fetches.clear();
                    }
                }
            }
            if let Some(shared_session) = self.shared_sessions.remove(&session_id) {
                self.shared_state.detach(shared_session);
            }
        }
        if let Some(shared_id) = self.shared_connections.remove(&connection_id) {
            self.shared_state.close_connection(shared_id);
        }
        self.cancel_actions_where(|action| action.connection_id == connection_id);
        let owned: Vec<String> = self
            .stream_connections
            .iter()
            .filter_map(|(handle, owner)| (*owner == connection_id).then_some(handle.clone()))
            .collect();
        for handle in owned {
            self.stream_connections.remove(&handle);
            self.streams.remove(&handle);
        }
        Ok(())
    }

    /// Process one CDP JSON message. State-only commands return a normal CDP
    /// response. Host-backed commands return a response containing an opaque
    /// `obscuraAction`; call `completeAction` to finish it.
    #[wasm_bindgen(js_name = cdpRequest)]
    pub fn cdp_request(&mut self, connection_id: u32, message: &str) -> Result<String, JsValue> {
        bounded(message, MAX_MESSAGE_BYTES, "CDP message")?;
        let request: Request = match serde_json::from_str(message) {
            Ok(request) => request,
            Err(error) => {
                return Ok(serde_json::to_string(&cdp_error_response(
                    &Value::Null,
                    -32700,
                    format!("Invalid CDP JSON: {error}"),
                    None,
                ))
                .expect("CDP error is serializable"));
            }
        };
        bounded(&request.method, MAX_METHOD_BYTES, "CDP method")?;
        if let Some(session) = request.session_id.as_deref() {
            bounded(session, MAX_SESSION_BYTES, "CDP session ID")?;
        }
        let response = self.dispatch(connection_id, request);
        let text = serde_json::to_string(&response)
            .map_err(|error| js_error(&format!("CDP response serialization failed: {error}")))?;
        bounded(&text, MAX_MESSAGE_BYTES, "CDP response")?;
        Ok(text)
    }

    /// Complete a host action and return its CDP response. The result must be
    /// a JSON value produced by the host, not an arbitrary JavaScript object.
    #[wasm_bindgen(js_name = completeAction)]
    pub fn complete_action(&mut self, action_id: u32, result_json: &str) -> Result<String, JsValue> {
        bounded(result_json, MAX_ACTION_RESULT_BYTES, "CDP action result")?;
        let Some(action) = self.actions.get(&action_id).cloned() else {
            return Err(js_error("stale or unknown CDP action"));
        };
        // Actions are capabilities for a live target session, not durable work
        // items. A host can race a completion with detach/close, so validate
        // the ownership again immediately before accepting its result.
        if !self.action_is_live(&action) {
            self.actions.remove(&action_id);
            let _ = self.action_queue.cancel(action_id);
            return Err(js_error("CDP action target session is no longer live"));
        }
        let mut result: Value = serde_json::from_str(result_json)
            .map_err(|error| js_error(&format!("invalid CDP action result: {error}")))?;
        let generation = self.action_queue.generation();
        self.action_queue
            .complete(&HostActionResult {
                action_id,
                generation,
                ok: true,
                value: Value::Null,
            })
            .map_err(|_| js_error("stale or unknown CDP action"))?;
        let shared_action_id = self
            .shared_actions
            .remove(&action_id)
            .ok_or_else(|| js_error("shared CDP action is missing"))?;
        let shared_result = if let Some(error) = result.get("error").and_then(Value::as_object) {
            let message = error.get("message").and_then(Value::as_str).unwrap_or("Portable host action failed");
            EngineActionResult::Failed(CdpFailure::host(message))
        } else {
            EngineActionResult::Value(result.clone())
        };
        match self.shared_state.complete_action(shared_action_id, shared_result) {
            Ok(()) | Err(CdpFailure::Host(_)) => {}
            Err(error) => return Err(js_error(&format!("shared CDP action completion failed: {error}"))),
        }
        self.actions.remove(&action_id);
        if action.kind == "navigate" || action.kind == "reload" || action.kind == "setDocumentContent" {
            if let Some(target) = self.targets.get_mut(&action.target_id) {
                if let Some(url) = result.get("url").and_then(Value::as_str) {
                    target.url = url.to_string();
                }
                if let Some(loader_id) = result.get("loaderId").and_then(Value::as_str) {
                    target.loader_id = loader_id.to_string();
                }
                if let Some(title) = result.get("title").and_then(Value::as_str) {
                    target.title = title.to_string();
                }
                if let Some(state) = result.get("__obscuraState").and_then(Value::as_object) {
                    if let Some(url) = state.get("url").and_then(Value::as_str) {
                        target.url = url.to_string();
                    }
                    if let Some(loader_id) = state.get("loaderId").and_then(Value::as_str) {
                        target.loader_id = loader_id.to_string();
                    }
                    if let Some(title) = state.get("title").and_then(Value::as_str) {
                        target.title = title.to_string();
                    }
                    if let Some(document_handle) = state.get("documentHandle").and_then(Value::as_u64) {
                        target.document_handle = u32::try_from(document_handle).map_err(|_| js_error("document handle exceeds u32"))?;
                    }
                    if let Some(revision) = state.get("revision").and_then(Value::as_u64) {
                        target.revision = u32::try_from(revision).map_err(|_| js_error("page revision exceeds u32"))?;
                    }
                    if let Some(html) = state.get("html").and_then(Value::as_str) {
                        target
                            .core
                            .set_html(html)
                            .map_err(|_| js_error("portable CDP document replacement failed"))?;
                        target
                            .core
                            .set_document_metadata(&target.url, "", "UTF-8")
                            .map_err(|_| js_error("portable CDP document metadata update failed"))?;
                        target.document_handle = target.core.document_handle();
                        target.revision = target.core.page_revision();
                    }
                }
            }
        }
        if action.kind == "navigate" || action.kind == "reload" || action.kind == "setDocumentContent" {
            if let (Some(shared_page), Some(target)) = (
                self.shared_pages.get(&action.target_id).copied(),
                self.targets.get(&action.target_id),
            ) {
                let _ = self.shared_state.update_page(
                    &shared_page,
                    Some(&target.url),
                    Some(&target.title),
                    Some(&target.loader_id),
                    Some(u64::from(target.revision)),
                );
            }
        }
        let mut network_metadata = None;
        if let Value::Object(ref mut object) = result {
            network_metadata = object.remove("__obscuraNetwork");
        }
        if let Some(network_metadata) = network_metadata {
            self.record_network_metadata(&action.target_id, &network_metadata)?;
        }
        let response = if let Value::Object(mut object) = result {
            object.remove("__obscuraState");
            if let Some(error) = object.get("error").and_then(Value::as_object) {
                let code = error.get("code").and_then(Value::as_i64).and_then(|value| i32::try_from(value).ok()).unwrap_or(-32603);
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Portable host action failed");
                cdp_error_response_with_data(
                    &action.request_id,
                    code,
                    message,
                    error.get("data"),
                    action.session_id.as_deref(),
                )
            } else {
                cdp_result_response(&action.request_id, Value::Object(object), action.session_id.as_deref())
            }
        } else {
            cdp_result_response(&action.request_id, result, action.session_id.as_deref())
        };
        Ok(serde_json::to_string(&response)
            .map_err(|error| js_error(&format!("CDP response serialization failed: {error}")))?)
    }

    /// Record host-owned network activity which completed after the original
    /// CDP action returned. Page `fetch()`/XHR work is asynchronous, so it
    /// cannot always be attached to a pending action. Keeping this ingress in
    /// the portable core preserves the same event routing and response-body
    /// ownership as navigation actions.
    #[wasm_bindgen(js_name = recordNetworkMetadata)]
    pub fn record_network_metadata_json(
        &mut self,
        target_id: &str,
        metadata_json: &str,
    ) -> Result<(), JsValue> {
        bounded(target_id, MAX_METHOD_BYTES, "CDP target ID")?;
        bounded(metadata_json, MAX_ACTION_RESULT_BYTES, "CDP network metadata")?;
        let metadata: Value = serde_json::from_str(metadata_json)
            .map_err(|error| js_error(&format!("invalid CDP network metadata: {error}")))?;
        self.record_network_metadata(target_id, &metadata)
    }

    /// Ask the portable CDP state whether a host-owned request must pause for
    /// Fetch interception. The host supplies only bounded, clone-safe request
    /// metadata; the returned resolution is drained after a CDP client calls
    /// Fetch.continueRequest/fulfillRequest/failRequest.
    #[wasm_bindgen(js_name = interceptFetchRequest)]
    pub fn intercept_fetch_request_json(
        &mut self,
        target_id: &str,
        metadata_json: &str,
    ) -> Result<String, JsValue> {
        bounded(target_id, MAX_METHOD_BYTES, "CDP target ID")?;
        bounded(metadata_json, MAX_ACTION_RESULT_BYTES, "Fetch request metadata")?;
        let metadata: Value = serde_json::from_str(metadata_json)
            .map_err(|error| js_error(&format!("invalid Fetch request metadata: {error}")))?;
        let result = self.intercept_fetch_request(target_id, &metadata)?;
        serde_json::to_string(&result)
            .map_err(|error| js_error(&format!("Fetch interception result serialization failed: {error}")))
    }

    /// Drain host-facing Fetch resolutions produced by portable CDP commands.
    /// Each entry is consumed exactly once by the Node transport adapter.
    #[wasm_bindgen(js_name = drainFetchResolutions)]
    pub fn drain_fetch_resolutions_json(&mut self) -> Result<String, JsValue> {
        let mut resolutions = Vec::new();
        let shared_pages: Vec<PageId> = self.shared_pages.values().copied().collect();
        for page_id in shared_pages {
            let remaining = MAX_FETCH_RESOLUTIONS.saturating_sub(resolutions.len());
            if remaining == 0 {
                break;
            }
            resolutions.extend(self.shared_state.drain_fetch_resolutions(&page_id, remaining));
        }
        while let Some(resolution) = self.fetch_resolutions.pop_front() {
            resolutions.push(resolution);
        }
        let text = serde_json::to_string(&resolutions)
            .map_err(|error| js_error(&format!("Fetch resolution serialization failed: {error}")))?;
        bounded(&text, MAX_EVENT_BYTES, "Fetch resolution batch")?;
        Ok(text)
    }

    /// Return the target-neutral cache policy selected by Network commands.
    /// The host owns byte storage, while the portable core owns this policy
    /// bit and therefore remains the source of truth for CDP state.
    #[wasm_bindgen(js_name = cacheDisabled)]
    pub fn cache_disabled_json(&self, target_id: &str) -> Result<bool, JsValue> {
        bounded(target_id, MAX_METHOD_BYTES, "CDP target ID")?;
        let page_id = self
            .shared_pages
            .get(target_id)
            .ok_or_else(|| js_error("portable cache target is not known"))?;
        self.shared_state
            .cache_disabled(page_id)
            .map_err(|error| js_error(&format!("portable cache target is not known: {error}")))
    }

    /// Clear response-body ownership associated with a target. The host
    /// adapter clears its bounded HTTP cache when it receives the same CDP
    /// command; this method keeps the portable response state coherent.
    #[wasm_bindgen(js_name = clearResponseCache)]
    pub fn clear_response_cache(&mut self, target_id: &str) -> Result<(), JsValue> {
        bounded(target_id, MAX_METHOD_BYTES, "CDP target ID")?;
        let page_id = self
            .shared_pages
            .get(target_id)
            .copied()
            .ok_or_else(|| js_error("portable cache target is not known"))?;
        self.shared_state
            .clear_response_bodies(&page_id)
            .map_err(|error| js_error(&format!("portable cache target is not known: {error}")))?;
        let Some(target) = self.targets.get_mut(target_id) else {
            return Err(js_error("portable cache target is not known"));
        };
        target.response_bodies.clear();
        Ok(())
    }

    /// Remove a paused request when the host page is reset or the page-side
    /// fetch is aborted before a CDP client responds.
    #[wasm_bindgen(js_name = cancelFetchRequest)]
    pub fn cancel_fetch_request_json(&mut self, target_id: &str, request_id: &str) -> Result<(), JsValue> {
        bounded(target_id, MAX_METHOD_BYTES, "CDP target ID")?;
        bounded(request_id, MAX_METHOD_BYTES, "Fetch request ID")?;
        if let Some(shared_page) = self.shared_pages.get(target_id).copied() {
            self.shared_state
                .cancel_paused_request(&shared_page, request_id)
                .map_err(|error| js_error(&format!("portable Fetch target is not known: {error}")))?;
        }
        if let Some(target) = self.targets.get_mut(target_id) {
            target.paused_fetches.remove(request_id);
        }
        Ok(())
    }

    /// Return and remove up to `max_items` queued events for a connection.
    #[wasm_bindgen(js_name = pollCdpEvents)]
    pub fn poll_cdp_events(&mut self, connection_id: u32, max_items: u32) -> Result<String, JsValue> {
        let Some(connection) = self.connections.get_mut(&connection_id) else {
            return Err(js_error("unknown CDP connection"));
        };
        let max_items = usize::try_from(max_items)
            .map_err(|_| js_range_error("event count is not representable"))?;
        let mut events = Vec::with_capacity(max_items.min(MAX_EVENT_QUEUE));
        for _ in 0..max_items.min(MAX_EVENT_QUEUE) {
            let Some(event) = connection.events.pop_front() else {
                break;
            };
            events.push(event);
        }
        let text = serde_json::to_string(&events)
            .map_err(|error| js_error(&format!("CDP event serialization failed: {error}")))?;
        bounded(&text, MAX_EVENT_BYTES, "CDP event batch")?;
        Ok(text)
    }

    /// Return machine-readable state/capabilities for host negotiation.
    #[wasm_bindgen(js_name = cdpStatus)]
    pub fn cdp_status(&self) -> String {
        json!({
            "cdpAbiVersion": CDP_ABI_VERSION,
            "targets": self.targets.len(),
            "connections": self.connections.len(),
            "pendingActions": self.action_queue.pending_len(),
            "eventQueueLimit": MAX_EVENT_QUEUE,
            "actionResultBytes": MAX_ACTION_RESULT_BYTES,
            "streamBytes": MAX_STREAM_BYTES,
            "streamChunkBytes": MAX_STREAM_CHUNK_BYTES,
            "streams": self.streams.len(),
            "responseBodyBytes": MAX_RESPONSE_BODY_BYTES,
            "responseBodies": self.targets.values().map(|target| target.response_bodies.len()).sum::<usize>(),
            "fetchPatternLimit": MAX_FETCH_PATTERNS,
            "fetchPausedRequestLimit": MAX_FETCH_REQUESTS,
            "fetchResolutionQueue": self.fetch_resolutions.len() + self.shared_state.fetch_resolution_count(),
        })
        .to_string()
    }
}

impl PortableCdp {
    fn record_network_metadata(&mut self, target_id: &str, metadata: &Value) -> Result<(), JsValue> {
        let Some(events) = metadata.as_array() else {
            return Err(js_error("portable network metadata must be an array"));
        };
        if events.len() > MAX_EVENT_QUEUE {
            return Err(js_range_error("portable network metadata exceeds the event limit"));
        }
        let shared_page = self.shared_pages.get(target_id).copied();
        let mut shared_bodies = Vec::new();
        let Some(target) = self.targets.get_mut(target_id) else {
            return Err(js_error("portable network target is not known"));
        };
        for event in events {
            let Some(event) = event.as_object() else {
                return Err(js_error("portable network event must be an object"));
            };
            let request_id = event.get("requestId").and_then(Value::as_str).unwrap_or("");
            let loader_id = event.get("loaderId").and_then(Value::as_str).unwrap_or("");
            let url = event.get("url").and_then(Value::as_str).unwrap_or("");
            let method = event.get("method").and_then(Value::as_str).unwrap_or("GET");
            if request_id.is_empty() || request_id.len() > MAX_METHOD_BYTES || url.len() > MAX_NAVIGATION_URL_BYTES {
                return Err(js_error("portable network event has an invalid request identity"));
            }
            let response_headers = event.get("responseHeaders").cloned().unwrap_or_else(|| Value::Object(Map::new()));
            if !response_headers.is_object() {
                return Err(js_error("portable network response headers must be an object"));
            }
            let body_base64 = event.get("bodyBase64").and_then(Value::as_str);
            if let Some(body_base64) = body_base64 {
                bounded(body_base64, MAX_ACTION_RESULT_BYTES, "portable network response body")?;
                let body = BASE64
                    .decode(body_base64)
                    .map_err(|_| js_error("portable network response body is not valid base64"))?;
                if body.len() > MAX_RESPONSE_BODY_BYTES {
                    return Err(js_range_error("portable network response body exceeds the 4MiB limit"));
                }
            }
            if target.response_bodies.len() >= MAX_RESPONSE_BODIES && !target.response_bodies.contains_key(request_id) {
                let oldest = target.response_bodies.keys().next().cloned();
                if let Some(oldest) = oldest {
                    target.response_bodies.remove(&oldest);
                }
            }
            if let Some(body_base64) = body_base64 {
                target.response_bodies.insert(request_id.to_string(), ResponseBody {
                    body: body_base64.to_string(),
                    base64_encoded: true,
                });
                shared_bodies.push((request_id.to_string(), body_base64.to_string(), true));
            }

            let request_headers = event.get("requestHeaders").cloned().unwrap_or_else(|| Value::Object(Map::new()));
            let timestamp = event.get("timestamp").and_then(Value::as_f64).unwrap_or(0.0);
            let wall_time = event.get("wallTime").and_then(Value::as_f64).unwrap_or(timestamp);
            let status = event.get("status").and_then(Value::as_u64).unwrap_or(200);
            let mime_type = event.get("mimeType").and_then(Value::as_str).unwrap_or("");
            let resource_type = event.get("resourceType").and_then(Value::as_str).unwrap_or("Document");
            let frame_id = target.frame_id.clone();
            let document_url = target.url.clone();
            let session_ids: Vec<(u32, String)> = self
                .connections
                .iter()
                .flat_map(|(connection_id, connection)| {
                    connection.sessions.iter().filter_map(|(session_id, mapped_target)| {
                        (mapped_target == target_id && target.network_enabled_sessions.contains(session_id))
                            .then_some((*connection_id, session_id.clone()))
                    })
                })
                .collect();
            for (connection_id, session_id) in session_ids {
                let Some(connection) = self.connections.get_mut(&connection_id) else { continue; };
                queue_event(connection, "Network.requestWillBeSent", json!({
                    "requestId": request_id,
                    "loaderId": loader_id,
                    "documentURL": document_url,
                    "request": {"url": url, "method": method, "headers": request_headers},
                    "timestamp": timestamp,
                    "wallTime": wall_time,
                    "initiator": {"type": event.get("initiatorType").and_then(Value::as_str).unwrap_or("other")},
                    "type": resource_type,
                    "frameId": frame_id,
                }), Some(&session_id));
                queue_event(connection, "Network.responseReceived", json!({
                    "requestId": request_id,
                    "loaderId": loader_id,
                    "timestamp": timestamp,
                    "type": resource_type,
                    "response": {"url": url, "status": status, "statusText": "", "headers": response_headers, "mimeType": mime_type},
                    "frameId": frame_id,
                }), Some(&session_id));
                queue_event(connection, "Network.loadingFinished", json!({
                    "requestId": request_id,
                    "timestamp": timestamp,
                    "encodedDataLength": event.get("bodySize").and_then(Value::as_u64).unwrap_or(0),
                }), Some(&session_id));
            }
        }
        if let Some(shared_page) = shared_page {
            for (request_id, body, base64_encoded) in shared_bodies {
                self.shared_state
                    .store_response_body(&shared_page, &request_id, body, base64_encoded)
                    .map_err(|error| js_error(&format!("portable network response state failed: {error}")))?;
            }
        }
        Ok(())
    }

    fn intercept_fetch_request(&mut self, target_id: &str, metadata: &Value) -> Result<Value, JsValue> {
        let Some(metadata) = metadata.as_object() else {
            return Err(js_error("Fetch request metadata must be an object"));
        };
        let request_id = metadata.get("requestId").and_then(Value::as_str).unwrap_or("");
        let url = metadata.get("url").and_then(Value::as_str).unwrap_or("");
        let method = metadata.get("method").and_then(Value::as_str).unwrap_or("GET");
        if request_id.is_empty() || request_id.len() > MAX_METHOD_BYTES {
            return Err(js_error("Fetch requestId is invalid"));
        }
        bounded(url, MAX_NAVIGATION_URL_BYTES, "Fetch request URL")?;
        bounded(method, 32, "Fetch request method")?;
        let request_stage = metadata
            .get("requestStage")
            .and_then(Value::as_str)
            .unwrap_or("Request");
        if request_stage != "Request" && request_stage != "Response" {
            return Err(js_error("Fetch requestStage must be Request or Response"));
        }
        let shared_page = self.shared_pages.get(target_id).copied();
        let shared_response_body = if request_stage == "Response" {
            if let Some(body_base64) = metadata.get("responseBodyBase64").and_then(Value::as_str) {
                bounded(body_base64, MAX_ACTION_RESULT_BYTES, "Fetch response body")?;
                let body = BASE64
                    .decode(body_base64)
                    .map_err(|_| js_error("Fetch response body is not valid base64"))?;
                if body.len() > MAX_RESPONSE_BODY_BYTES {
                    return Err(js_range_error("Fetch response body exceeds the 4MiB limit"));
                }
                Some(body_base64.to_string())
            } else {
                None
            }
        } else {
            None
        };
        if let (Some(shared_page), Some(body)) = (shared_page, shared_response_body.as_ref()) {
            self.shared_state
                .store_response_body(&shared_page, request_id, body.clone(), true)
                .map_err(|error| js_error(&format!("portable Fetch response state failed: {error}")))?;
        }
        let Some(target) = self.targets.get_mut(target_id) else {
            return Err(js_error("portable Fetch target is not known"));
        };
        if target.paused_fetches.len() >= MAX_FETCH_REQUESTS && !target.paused_fetches.contains_key(request_id) {
            return Err(js_range_error("portable Fetch paused-request limit exceeded"));
        }
        let request_headers = metadata.get("headers").cloned().unwrap_or_else(|| Value::Object(Map::new()));
        let Some(request_headers) = request_headers.as_object() else {
            return Err(js_error("Fetch request headers must be an object"));
        };
        let mut headers = BTreeMap::new();
        for (name, value) in request_headers {
            let Some(value) = value.as_str() else {
                return Err(js_error("Fetch request header values must be strings"));
            };
            bounded(name, 1024, "Fetch request header name")?;
            bounded(value, MAX_NAVIGATION_HEADERS_BYTES, "Fetch request header value")?;
            headers.insert(name.clone(), value.to_string());
        }
        let resource_type = metadata
            .get("resourceType")
            .and_then(Value::as_str)
            .unwrap_or("Fetch");
        bounded(resource_type, 64, "Fetch resource type")?;
        let post_data = metadata.get("postData").and_then(Value::as_str).map(str::to_string);
        if let Some(post_data) = post_data.as_deref() {
            bounded(post_data, MAX_ACTION_RESULT_BYTES, "Fetch postData")?;
        }
        let frame_id = metadata
            .get("frameId")
            .and_then(Value::as_str)
            .unwrap_or(&target.frame_id)
            .to_string();
        if request_stage == "Response" {
            if let Some(body_base64) = metadata.get("responseBodyBase64").and_then(Value::as_str) {
                bounded(body_base64, MAX_ACTION_RESULT_BYTES, "Fetch response body")?;
                let body = BASE64
                    .decode(body_base64)
                    .map_err(|_| js_error("Fetch response body is not valid base64"))?;
                if body.len() > MAX_RESPONSE_BODY_BYTES {
                    return Err(js_range_error("Fetch response body exceeds the 4MiB limit"));
                }
                if target.response_bodies.len() >= MAX_RESPONSE_BODIES
                    && !target.response_bodies.contains_key(request_id)
                {
                    let oldest = target.response_bodies.keys().next().cloned();
                    if let Some(oldest) = oldest {
                        target.response_bodies.remove(&oldest);
                    }
                }
                target.response_bodies.insert(request_id.to_string(), ResponseBody {
                    body: body_base64.to_string(),
                    base64_encoded: true,
                });
            }
        }
        let matching_sessions: Vec<(u32, String)> = self
            .connections
            .iter()
            .flat_map(|(connection_id, connection)| {
                connection.sessions.iter().filter_map(|(session_id, mapped_target)| {
                    if mapped_target != target_id {
                        return None;
                    }
                    let patterns = target.fetch_patterns.get(session_id)?;
                    patterns.iter().any(|pattern| {
                        pattern.request_stage == request_stage
                            && fetch_url_matches(&pattern.url_pattern, url)
                    })
                        .then_some((*connection_id, session_id.clone()))
                })
            })
            .collect();
        if matching_sessions.is_empty() {
            return Ok(json!({"paused": false, "requestId": request_id}));
        }
        if let Some(shared_page) = shared_page {
            self.shared_state
                .register_paused_request(&shared_page, request_id)
                .map_err(|error| js_error(&format!("portable Fetch paused-request state failed: {error}")))?;
        }
        let paused = PausedFetch {
            request_id: request_id.to_string(),
            url: url.to_string(),
            method: method.to_string(),
            headers,
            post_data,
            resource_type: resource_type.to_string(),
            frame_id,
            request_stage: request_stage.to_string(),
        };
        target.paused_fetches.insert(request_id.to_string(), paused);
        let request = target.paused_fetches.get(request_id).expect("paused Fetch was inserted");
        let request_payload = json!({
            "url": request.url,
            "method": request.method,
            "headers": request.headers,
            "postData": request.post_data,
        });
        let response_headers = metadata
            .get("responseHeaders")
            .and_then(Value::as_object)
            .map(|headers| {
                headers
                    .iter()
                    .map(|(name, value)| json!({"name": name, "value": value.as_str().unwrap_or("")}))
                    .collect::<Vec<_>>()
            });
        let response_status = metadata.get("responseStatusCode").and_then(Value::as_u64);
        for (connection_id, session_id) in &matching_sessions {
            if let Some(connection) = self.connections.get_mut(connection_id) {
                let mut paused_event = json!({
                    "requestId": request.request_id,
                    "request": request_payload,
                    "resourceType": request.resource_type,
                    "frameId": request.frame_id,
                    "networkId": request.request_id,
                });
                if request.request_stage == "Response" {
                    if let Some(status) = response_status {
                        paused_event["responseStatusCode"] = json!(status);
                    }
                    if let Some(headers) = response_headers.clone() {
                        paused_event["responseHeaders"] = json!(headers);
                    }
                }
                queue_event(connection, "Fetch.requestPaused", paused_event, Some(session_id));
            }
        }
        Ok(json!({"paused": true, "requestId": request_id, "sessionCount": matching_sessions.len()}))
    }

    fn resolve_fetch_request(&mut self, target_id: &str, request_id: &str, resolution: Value) -> Result<(), String> {
        let Some(target) = self.targets.get_mut(target_id) else {
            return Err("target is not known".to_string());
        };
        if target.paused_fetches.remove(request_id).is_none() {
            // CDP treats a stale resolution as an idempotent no-op for a
            // request which already completed or was canceled by the host.
            return Ok(());
        }
        if self.fetch_resolutions.len() >= MAX_FETCH_RESOLUTIONS {
            return Err("Fetch resolution queue is full".to_string());
        }
        self.fetch_resolutions.push_back(resolution);
        Ok(())
    }

    fn disable_fetch_for_session(&mut self, target_id: &str, session_id: &str) -> Result<(), String> {
        let Some(target) = self.targets.get_mut(target_id) else {
            return Err("target is not known".to_string());
        };
        target.fetch_patterns.remove(session_id);
        if target.fetch_patterns.is_empty() {
            let request_ids: Vec<String> = target.paused_fetches.keys().cloned().collect();
            for request_id in request_ids {
                let _ = self.resolve_fetch_request(target_id, &request_id, json!({
                    "requestId": request_id,
                    "action": "continue",
                }));
            }
        }
        Ok(())
    }

    fn queue_fetch_continue(&mut self, target_id: &str, request_id: &str, params: &Map<String, Value>) -> Result<(), String> {
        let url = params.get("url").and_then(Value::as_str);
        let method = params.get("method").and_then(Value::as_str);
        let post_data = params.get("postData").and_then(Value::as_str);
        if let Some(url) = url {
            if url.len() > MAX_NAVIGATION_URL_BYTES {
                return Err("Fetch continue URL exceeds the byte limit".to_string());
            }
        }
        if let Some(method) = method {
            if method.len() > 32 {
                return Err("Fetch continue method exceeds the byte limit".to_string());
            }
        }
        if let Some(post_data) = post_data {
            if post_data.len() > MAX_ACTION_RESULT_BYTES {
                return Err("Fetch continue postData exceeds the byte limit".to_string());
            }
        }
        let headers = params.get("headers").cloned().unwrap_or(Value::Null);
        if !headers.is_null() {
            validate_fetch_header_array(&headers)?;
        }
        self.resolve_fetch_request(target_id, request_id, json!({
            "requestId": request_id,
            "action": "continue",
            "url": url,
            "method": method,
            "headers": headers,
            "postData": post_data,
        }))
    }

    fn queue_fetch_fulfill(&mut self, target_id: &str, request_id: &str, params: &Map<String, Value>) -> Result<(), String> {
        let status = params.get("responseCode").and_then(Value::as_u64).unwrap_or(200);
        if status > u16::MAX as u64 {
            return Err("responseCode is out of range".to_string());
        }
        let headers = params.get("responseHeaders").cloned().unwrap_or_else(|| Value::Array(Vec::new()));
        validate_fetch_header_array(&headers)?;
        let body = params.get("body").and_then(Value::as_str).unwrap_or("");
        bounded_js_string(body, MAX_ACTION_RESULT_BYTES, "Fetch fulfill body")?;
        if body.len() > MAX_RESPONSE_BODY_BYTES.saturating_mul(2) {
            return Err("Fetch fulfill body exceeds the response limit".to_string());
        }
        let decoded = BASE64.decode(body).map_err(|_| "Fetch fulfill body is not valid base64".to_string())?;
        if decoded.len() > MAX_RESPONSE_BODY_BYTES {
            return Err("Fetch fulfill body exceeds the response limit".to_string());
        }
        self.resolve_fetch_request(target_id, request_id, json!({
            "requestId": request_id,
            "action": "fulfill",
            "status": status,
            "headers": headers,
            "bodyBase64": body,
        }))
    }

    fn queue_fetch_fail(&mut self, target_id: &str, request_id: &str, params: &Map<String, Value>) -> Result<(), String> {
        let reason = params.get("errorReason").and_then(Value::as_str).unwrap_or("Failed");
        bounded_js_string(reason, 256, "Fetch errorReason")?;
        self.resolve_fetch_request(target_id, request_id, json!({
            "requestId": request_id,
            "action": "fail",
            "reason": reason,
        }))
    }

    fn target_for_session(
        &self,
        connection_id: u32,
        session_id: Option<&str>,
    ) -> Result<(String, Option<String>), Value> {
        let Some(connection) = self.connections.get(&connection_id) else {
            return Err(cdp_error_response(&Value::Null, -32000, "unknown CDP connection", None));
        };
        let Some(session_id) = session_id else {
            return Err(cdp_error_response(&Value::Null, -32000, "a target session is required", None));
        };
        if session_id == connection.browser_session {
            return Err(cdp_error_response(&Value::Null, -32000, "browser session has no page target", Some(session_id)));
        }
        let Some(target_id) = connection.sessions.get(session_id) else {
            return Err(cdp_error_response(&Value::Null, -32000, "unknown target session", Some(session_id)));
        };
        Ok((target_id.clone(), Some(session_id.to_string())))
    }

    fn dispatch(&mut self, connection_id: u32, request: Request) -> Value {
        if !self.connections.contains_key(&connection_id) {
            return cdp_error_response(&request.id, -32000, "unknown CDP connection", request.session_id.as_deref());
        }
        if request.session_id.is_none() {
            return self.dispatch_browser(connection_id, request);
        }
        let (target_id, session_id) = match self.target_for_session(connection_id, request.session_id.as_deref()) {
            Ok(value) => value,
            Err(mut error) => {
                if let Value::Object(ref mut object) = error {
                    object.insert("id".to_string(), request.id.clone());
                }
                return error;
            }
        };
        self.dispatch_page(connection_id, target_id, session_id, request)
    }

    fn dispatch_browser(&mut self, connection_id: u32, request: Request) -> Value {
        let session = request.session_id.as_deref();
        if matches!(
            request.method.as_str(),
            "Browser.getVersion"
                | "Browser.getWindowForTarget"
                | "Browser.getWindowBounds"
                | "Browser.setWindowBounds"
                | "Browser.setDownloadBehavior"
                | "Target.getBrowserContexts"
                | "Target.getTargets"
                | "Target.createBrowserContext"
                | "Target.disposeBrowserContext"
                | "Target.createTarget"
                | "Target.closeTarget"
        ) {
            if let Some(shared_response) = self.shared_target_response(&request) {
                return shared_response;
            }
        }
        match request.method.as_str() {
            "Browser.getVersion" => cdp_result_response(
                &request.id,
                json!({
                    "protocolVersion": "1.3",
                    "product": "Obscura/WASM",
                    "revision": "portable",
                    "userAgent": "Obscura-WASM",
                    "jsVersion": "host",
                }),
                session,
            ),
            "Target.getBrowserContexts" => cdp_result_response(
                &request.id,
                json!({"browserContextIds": self.contexts.iter().filter(|id| id.as_str() != "default").collect::<Vec<_>>() }),
                session,
            ),
            "Target.createBrowserContext" => {
                let shared_id = match self.shared_state.create_context(ContextOptions::default()) {
                    Ok(id) => id,
                    Err(error) => return cdp_error_response(&request.id, -32000, error.to_string(), session),
                };
                let id = format!("context-{}", shared_id.get());
                self.shared_contexts.insert(id.clone(), shared_id);
                self.contexts.insert(id.clone());
                cdp_result_response(&request.id, json!({"browserContextId": id}), session)
            }
            "Target.disposeBrowserContext" => {
                let Some(id) = request.params.get("browserContextId").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "browserContextId is required", session);
                };
                if id == "default" || !self.contexts.contains(id) {
                    return cdp_error_response(&request.id, -32000, "browser context cannot be disposed", session);
                }
                let Some(shared_id) = self.shared_contexts.get(id).copied() else {
                    return cdp_error_response(&request.id, -32000, "shared browser context was not found", session);
                };
                let doomed: Vec<String> = self
                    .targets
                    .values()
                    .filter(|target| target.context_id == id)
                    .map(|target| target.id.clone())
                    .collect();
                if let Err(error) = self.shared_state.dispose_context(&shared_id) {
                    return cdp_error_response(&request.id, -32000, error.to_string(), session);
                }
                self.contexts.remove(id);
                self.shared_contexts.remove(id);
                for target_id in doomed {
                    self.destroy_target(&target_id);
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "Target.getTargets" => {
                let infos: Vec<Value> = self.targets.values().map(Self::target_info).collect();
                cdp_result_response(&request.id, json!({"targetInfos": infos}), session)
            }
            "Target.setDiscoverTargets" => {
                let discover = request.params.get("discover").and_then(Value::as_bool).unwrap_or(false);
                if let Some(connection) = self.connections.get_mut(&connection_id) {
                    connection.discover_targets = discover;
                    if discover {
                        let infos: Vec<Value> = self.targets.values().map(Self::target_info).collect();
                        for info in infos {
                            queue_event(connection, "Target.targetCreated", json!({"targetInfo": info}), None);
                        }
                    }
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "Target.setAutoAttach" => {
                let auto_attach = request.params.get("autoAttach").and_then(Value::as_bool).unwrap_or(false);
                if let Some(connection) = self.connections.get_mut(&connection_id) {
                    connection.auto_attach = auto_attach;
                }
                // Target auto-attachment is an ongoing subscription. Enabling
                // it must also attach the targets which were already alive,
                // otherwise a client can miss its initial page forever.
                if auto_attach {
                    self.auto_attach_existing_targets(connection_id);
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "Target.attachToBrowserTarget" => {
                let browser_session = self.connections.get(&connection_id).map(|c| c.browser_session.clone()).unwrap_or_default();
                cdp_result_response(&request.id, json!({"sessionId": browser_session}), session)
            }
            "Target.attachToTarget" => {
                let Some(target_id) = request.params.get("targetId").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "targetId is required", session);
                };
                if !self.targets.contains_key(target_id) {
                    return cdp_error_response(&request.id, -32602, "targetId is not known", session);
                }
                let session_id = self.allocate_session(connection_id, target_id);
                let info = self.targets.get(target_id).map(Self::target_info).unwrap_or(Value::Null);
                if let Some(connection) = self.connections.get_mut(&connection_id) {
                    queue_event(
                        connection,
                        "Target.attachedToTarget",
                        json!({"sessionId": session_id, "targetInfo": info, "waitingForDebugger": false}),
                        None,
                    );
                }
                cdp_result_response(&request.id, json!({"sessionId": session_id}), session)
            }
            "Target.detachFromTarget" => {
                let Some(session_id) = request.params.get("sessionId").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "sessionId is required", session);
                };
                if self.detach_session(connection_id, session_id).is_none() {
                    return cdp_error_response(&request.id, -32000, "target session is not known", session);
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "Target.closeTarget" => {
                let Some(target_id) = request.params.get("targetId").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "targetId is required", session);
                };
                let exists = self.targets.contains_key(target_id);
                if exists {
                    self.destroy_target(target_id);
                }
                cdp_result_response(&request.id, json!({"success": exists}), session)
            }
            "Target.createTarget" => {
                let context_id = request.params.get("browserContextId").and_then(Value::as_str).unwrap_or("default");
                if !self.contexts.contains(context_id) {
                    return cdp_error_response(&request.id, -32000, "browser context was not found", session);
                }
                let url = request.params.get("url").and_then(Value::as_str).unwrap_or("about:blank");
                let target_id = self.create_target(context_id, url, "");
                cdp_result_response(&request.id, json!({"targetId": target_id}), session)
            }
            _ => cdp_error_response(&request.id, -32601, "method is not implemented", session),
        }
    }

    fn shared_target_response(&mut self, request: &Request) -> Option<Value> {
        let id = request.id.as_u64()?;
        let shared_request = obscura_cdp::protocol::CdpRequest {
            id,
            method: request.method.clone(),
            params: Value::Object(request.params.clone()),
            session_id: request.session_id.clone(),
        };
        let response = obscura_cdp::portable_target::dispatch(&shared_request, &mut self.shared_state);
        if response.error.is_none() {
            match request.method.as_str() {
                "Target.createBrowserContext" => {
                    if let Some(id) = response
                        .result
                        .as_ref()
                        .and_then(|value| value.get("browserContextId"))
                        .and_then(Value::as_str)
                    {
                        if let Some(shared_id) = id
                            .strip_prefix("context-")
                            .and_then(|value| value.parse::<u64>().ok())
                            .map(ContextId::new)
                        {
                            self.shared_contexts.insert(id.to_string(), shared_id);
                            self.contexts.insert(id.to_string());
                        }
                    }
                }
                "Target.disposeBrowserContext" => {
                    let context_id = request.params.get("browserContextId").and_then(Value::as_str);
                    let doomed: Vec<String> = context_id
                        .filter(|id| *id != "default")
                        .map(|id| {
                            self.targets
                                .values()
                                .filter(|target| target.context_id == id)
                                .map(|target| target.id.clone())
                                .collect()
                        })
                        .unwrap_or_default();
                    if let Some(id) = context_id {
                        self.contexts.remove(id);
                        self.shared_contexts.remove(id);
                    }
                    for target_id in doomed {
                        self.destroy_target(&target_id);
                    }
                }
                "Target.closeTarget" => {
                    if response
                        .result
                        .as_ref()
                        .and_then(|value| value.get("success"))
                        .and_then(Value::as_bool)
                        == Some(true)
                    {
                        if let Some(target_id) = request.params.get("targetId").and_then(Value::as_str) {
                            self.destroy_target(target_id);
                        }
                    }
                }
                "Target.createTarget" => {
                    let context_id = request
                        .params
                        .get("browserContextId")
                        .and_then(Value::as_str)
                        .unwrap_or("default");
                    let url = request
                        .params
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or("about:blank");
                    if let Some(target_id) = response
                        .result
                        .as_ref()
                        .and_then(|value| value.get("targetId"))
                        .and_then(Value::as_str)
                    {
                        if let Some(page_number) = target_id
                            .strip_prefix("page-")
                            .and_then(|value| value.parse::<u64>().ok())
                        {
                            self.create_target_with_shared_page(
                                context_id,
                                url,
                                "",
                                PageId::new(page_number),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        serde_json::to_value(response).ok()
    }

    fn shared_page_response(&self, request: &Request, target_id: &str) -> Option<Value> {
        if !obscura_cdp::portable_page::supports(&request.method) {
            return None;
        }
        let id = request.id.as_u64()?;
        let page_id = *self.shared_pages.get(target_id)?;
        let shared_request = obscura_cdp::protocol::CdpRequest {
            id,
            method: request.method.clone(),
            params: Value::Object(request.params.clone()),
            session_id: request.session_id.clone(),
        };
        let response = obscura_cdp::portable_page::dispatch(&shared_request, &self.shared_state, page_id);
        serde_json::to_value(response).ok()
    }

    fn shared_network_response(&mut self, request: &Request, target_id: &str) -> Option<Value> {
        if !obscura_cdp::portable_network::supports(&request.method) {
            return None;
        }
        let id = request.id.as_u64()?;
        let page_id = *self.shared_pages.get(target_id)?;
        let shared_session = request
            .session_id
            .as_ref()
            .and_then(|session| self.shared_sessions.get(session).copied());
        let shared_request = obscura_cdp::protocol::CdpRequest {
            id,
            method: request.method.clone(),
            params: Value::Object(request.params.clone()),
            session_id: request.session_id.clone(),
        };
        let response = obscura_cdp::portable_network::dispatch(
            &shared_request,
            &mut self.shared_state,
            page_id,
            shared_session,
        );
        if response.error.is_none() {
            if let Some(target) = self.targets.get_mut(target_id) {
                match request.method.as_str() {
                    "Network.enable" => {
                        if let Some(session) = request.session_id.as_ref() {
                            target.network_enabled_sessions.insert(session.clone());
                        }
                    }
                    "Network.disable" => {
                        if let Some(session) = request.session_id.as_ref() {
                            target.network_enabled_sessions.remove(session);
                        }
                        target.response_bodies.clear();
                    }
                    "Network.setCacheDisabled" => {
                        target.cache_disabled = request
                            .params
                            .get("cacheDisabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                    }
                    "Network.setExtraHTTPHeaders" => {
                        if let Some(headers) = request.params.get("headers").and_then(Value::as_object) {
                            target.extra_headers = headers
                                .iter()
                                .filter_map(|(name, value)| value.as_str().map(|value| (name.clone(), value.to_string())))
                                .collect();
                        }
                    }
                    "Network.clearBrowserCache" => target.response_bodies.clear(),
                    _ => {}
                }
            }
        }
        serde_json::to_value(response).ok()
    }

    fn shared_emulation_response(&mut self, request: &Request, target_id: &str) -> Option<Value> {
        if !obscura_cdp::portable_emulation::supports(&request.method) {
            return None;
        }
        let id = request.id.as_u64()?;
        let page_id = *self.shared_pages.get(target_id)?;
        let shared_request = obscura_cdp::protocol::CdpRequest {
            id,
            method: request.method.clone(),
            params: Value::Object(request.params.clone()),
            session_id: request.session_id.clone(),
        };
        let response = obscura_cdp::portable_emulation::dispatch(
            &shared_request,
            &mut self.shared_state,
            page_id,
        );
        if response.error.is_none() {
            if let Some(target) = self.targets.get_mut(target_id) {
                match request.method.as_str() {
                    "Emulation.setDeviceMetricsOverride" => {
                        if let (Some(width), Some(height)) = (
                            request.params.get("width").and_then(Value::as_u64),
                            request.params.get("height").and_then(Value::as_u64),
                        ) {
                            target.viewport = Viewport {
                                width: u32::try_from(width).unwrap_or(Viewport::default().width),
                                height: u32::try_from(height).unwrap_or(Viewport::default().height),
                            };
                        }
                    }
                    "Emulation.clearDeviceMetricsOverride" => {
                        target.viewport = Viewport::default();
                    }
                    "Emulation.setEmulatedMedia" => {
                        target.emulated_media = request
                            .params
                            .get("media")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                    }
                    "Emulation.setFocusEmulationEnabled" => {
                        target.focus_emulation = request
                            .params
                            .get("enabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                    }
                    _ => {}
                }
            }
        }
        serde_json::to_value(response).ok()
    }

    fn shared_fetch_response(&mut self, request: &Request, target_id: &str) -> Option<Value> {
        if !obscura_cdp::portable_fetch::supports(&request.method) {
            return None;
        }
        let id = request.id.as_u64()?;
        let page_id = *self.shared_pages.get(target_id)?;
        let shared_session = request
            .session_id
            .as_ref()
            .and_then(|session| self.shared_sessions.get(session).copied());
        let shared_request = obscura_cdp::protocol::CdpRequest {
            id,
            method: request.method.clone(),
            params: Value::Object(request.params.clone()),
            session_id: request.session_id.clone(),
        };
        let response = obscura_cdp::portable_fetch::dispatch(
            &shared_request,
            &mut self.shared_state,
            page_id,
            shared_session,
        );
        if response.error.is_none() {
            if let Some(target) = self.targets.get_mut(target_id) {
                match request.method.as_str() {
                    "Fetch.enable" => {
                        if let Some(session_id) = request.session_id.as_ref() {
                            if let Ok(patterns) = parse_fetch_patterns(&request.params) {
                                target.fetch_patterns.insert(session_id.clone(), patterns);
                            }
                        }
                    }
                    "Fetch.disable" => {
                        if let Some(session_id) = request.session_id.as_ref() {
                            target.fetch_patterns.remove(session_id);
                        }
                        if target.fetch_patterns.is_empty() {
                            target.paused_fetches.clear();
                        }
                    }
                    "Fetch.continueRequest" | "Fetch.fulfillRequest" | "Fetch.failRequest" => {
                        if let Some(request_id) = request.params.get("requestId").and_then(Value::as_str) {
                            target.paused_fetches.remove(request_id);
                        }
                    }
                    _ => {}
                }
            }
        }
        serde_json::to_value(response).ok()
    }

    fn dispatch_page(&mut self, connection_id: u32, target_id: String, session_id: Option<String>, request: Request) -> Value {
        if let Some(shared_response) = self.shared_fetch_response(&request, &target_id) {
            return shared_response;
        }
        if let Some(shared_response) = self.shared_emulation_response(&request, &target_id) {
            return shared_response;
        }
        if let Some(shared_response) = self.shared_network_response(&request, &target_id) {
            return shared_response;
        }
        if let Some(shared_response) = self.shared_page_response(&request, &target_id) {
            return shared_response;
        }
        let session = session_id.as_deref();
        match request.method.as_str() {
            "Runtime.enable" => {
                if let Some(connection) = self.connections.get_mut(&connection_id) {
                    let frame_id = self.targets.get(&target_id).map(|target| target.frame_id.clone()).unwrap_or_default();
                    let url = self.targets.get(&target_id).map(|target| target.url.clone()).unwrap_or_default();
                    queue_event(connection, "Runtime.executionContextCreated", json!({
                        "context": {"id": 1, "origin": url, "name": "", "uniqueId": format!("{target_id}:default"), "auxData": {"isDefault": true, "type": "default", "frameId": frame_id}}
                    }), session);
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "Page.enable" | "Page.disable" | "DOM.enable" | "DOM.disable" | "Runtime.disable" => {
                cdp_result_response(&request.id, json!({}), session)
            }
            "Fetch.enable" => {
                let Some(session_id) = session else {
                    return cdp_error_response(&request.id, -32600, "Fetch.enable requires a target session", session);
                };
                let patterns = match parse_fetch_patterns(&request.params) {
                    Ok(patterns) => patterns,
                    Err(error) => return cdp_error_response(&request.id, -32602, error, session),
                };
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                target.fetch_patterns.insert(session_id.to_string(), patterns);
                cdp_result_response(&request.id, json!({}), session)
            }
            "Fetch.disable" => {
                if let Some(session_id) = session {
                    if let Err(error) = self.disable_fetch_for_session(&target_id, session_id) {
                        return cdp_error_response(&request.id, -32000, error, session);
                    }
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "Fetch.continueRequest" => {
                let Some(session_id) = session else {
                    return cdp_error_response(&request.id, -32600, "Fetch.continueRequest requires a target session", session);
                };
                let Some(request_id) = request.params.get("requestId").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "requestId is required", session);
                };
                if let Err(error) = self.queue_fetch_continue(&target_id, request_id, &request.params) {
                    return cdp_error_response(&request.id, -32000, error, Some(session_id));
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "Fetch.fulfillRequest" => {
                let Some(request_id) = request.params.get("requestId").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "requestId is required", session);
                };
                if let Err(error) = self.queue_fetch_fulfill(&target_id, request_id, &request.params) {
                    return cdp_error_response(&request.id, -32000, error, session);
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "Fetch.failRequest" => {
                let Some(request_id) = request.params.get("requestId").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "requestId is required", session);
                };
                if let Err(error) = self.queue_fetch_fail(&target_id, request_id, &request.params) {
                    return cdp_error_response(&request.id, -32000, error, session);
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "Fetch.getResponseBody" => {
                let Some(request_id) = request.params.get("requestId").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "requestId is required", session);
                };
                let Some(target) = self.targets.get(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let Some(body) = target.response_bodies.get(request_id) else {
                    return cdp_error_response(&request.id, -32000, "No response body found for requestId", session);
                };
                cdp_result_response(&request.id, json!({"body": body.body, "base64Encoded": body.base64_encoded}), session)
            }
            "Network.enable" => {
                let Some(session_id) = session else {
                    return cdp_error_response(&request.id, -32600, "Network.enable requires a target session", session);
                };
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                target.network_enabled_sessions.insert(session_id.to_string());
                cdp_result_response(&request.id, json!({}), session)
            }
            "Network.disable" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                if let Some(session_id) = session {
                    target.network_enabled_sessions.remove(session_id);
                }
                target.response_bodies.clear();
                cdp_result_response(&request.id, json!({}), session)
            }
            "Page.getLayoutMetrics" => {
                let Some(target) = self.targets.get(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                cdp_result_response(&request.id, layout_metrics(target), session)
            }
            "Emulation.setDeviceMetricsOverride" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let width = match request.params.get("width").and_then(Value::as_u64) {
                    Some(value) if (1..=4096).contains(&value) => value as u32,
                    Some(_) => return cdp_error_response(&request.id, -32602, "width must be between 1 and 4096", session),
                    None => return cdp_error_response(&request.id, -32602, "width is required", session),
                };
                let height = match request.params.get("height").and_then(Value::as_u64) {
                    Some(value) if (1..=4096).contains(&value) => value as u32,
                    Some(_) => return cdp_error_response(&request.id, -32602, "height must be between 1 and 4096", session),
                    None => return cdp_error_response(&request.id, -32602, "height is required", session),
                };
                let device_scale_factor = request
                    .params
                    .get("deviceScaleFactor")
                    .and_then(Value::as_f64)
                    .unwrap_or(1.0);
                if !device_scale_factor.is_finite() || !(0.0..=8.0).contains(&device_scale_factor) {
                    return cdp_error_response(&request.id, -32602, "deviceScaleFactor must be between 0 and 8", session);
                }
                target.viewport = Viewport { width, height };
                cdp_result_response(&request.id, json!({}), session)
            }
            "Emulation.clearDeviceMetricsOverride" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                target.viewport = Viewport::default();
                cdp_result_response(&request.id, json!({}), session)
            }
            "Emulation.setEmulatedMedia" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let media = request.params.get("media").and_then(Value::as_str).unwrap_or("");
                if media.len() > 256 {
                    return cdp_error_response(&request.id, -32602, "media exceeds the 256-byte limit", session);
                }
                target.emulated_media = media.to_string();
                cdp_result_response(&request.id, json!({}), session)
            }
            "Emulation.setFocusEmulationEnabled" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                target.focus_emulation = request.params.get("enabled").and_then(Value::as_bool).unwrap_or(false);
                cdp_result_response(&request.id, json!({}), session)
            }
            "Network.setExtraHTTPHeaders" => {
                let headers = match parse_extra_headers(&request.params) {
                    Ok(headers) => headers,
                    Err(error) => return cdp_error_response(&request.id, -32602, error, session),
                };
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                target.extra_headers = headers;
                cdp_result_response(&request.id, json!({}), session)
            }
            "Network.getAllCookies" | "Storage.getCookies" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let now = match cookie_clock(&request.params) {
                    Ok(value) => value,
                    Err(error) => return cdp_error_response(&request.id, -32602, error, session),
                };
                let raw = match target.core.cdp_all_cookies(now) {
                    Ok(value) => value,
                    Err(error) => return cdp_error_response(&request.id, -32000, error, session),
                };
                let cookies = serde_json::from_str::<Value>(&raw).unwrap_or_else(|_| Value::Array(Vec::new()));
                cdp_result_response(&request.id, json!({"cookies": cookies}), session)
            }
            "Network.setCookies" | "Storage.setCookies" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let now = match cookie_clock(&request.params) {
                    Ok(value) => value,
                    Err(error) => return cdp_error_response(&request.id, -32602, error, session),
                };
                let import = match cdp_cookie_import(&request.params, &target.url) {
                    Ok(value) => value,
                    Err(error) => return cdp_error_response(&request.id, -32602, error, session),
                };
                if let Err(error) = target.core.cdp_import_cookies(&import, now) {
                    return cdp_error_response(&request.id, -32602, error, session);
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "Network.deleteCookies" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let Some(name) = request.params.get("name").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "name is required", session);
                };
                let domain = request.params.get("domain").and_then(Value::as_str).map(str::to_string).or_else(|| {
                    request.params.get("url").and_then(Value::as_str).and_then(cookie_url_host)
                }).unwrap_or_default();
                let path = request.params.get("path").and_then(Value::as_str).map(str::to_string);
                target.core.cdp_delete_cookies(name, &domain, path.as_deref());
                cdp_result_response(&request.id, json!({}), session)
            }
            "Network.clearBrowserCookies" | "Storage.clearDataForOrigin" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                target.core.cdp_clear_cookies();
                cdp_result_response(&request.id, json!({}), session)
            }
            "Network.setCacheDisabled" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                target.cache_disabled = request.params.get("cacheDisabled").and_then(Value::as_bool).unwrap_or(false);
                cdp_result_response(&request.id, json!({}), session)
            }
            "Network.clearBrowserCache" => {
                if let Some(target) = self.targets.get_mut(&target_id) {
                    target.response_bodies.clear();
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "Network.getResponseBody" => {
                let Some(request_id) = request.params.get("requestId").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "requestId is required", session);
                };
                if request_id.len() > MAX_METHOD_BYTES {
                    return cdp_error_response(&request.id, -32602, "requestId exceeds the 256-byte limit", session);
                }
                let Some(target) = self.targets.get(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let Some(body) = target.response_bodies.get(request_id) else {
                    return cdp_error_response(&request.id, -32000, "No response body found for requestId", session);
                };
                cdp_result_response(&request.id, json!({"body": body.body, "base64Encoded": body.base64_encoded}), session)
            }
            "Page.getFrameTree" => {
                let Some(target) = self.targets.get(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                cdp_result_response(&request.id, json!({"frameTree": {"frame": {
                    "id": target.frame_id,
                    "loaderId": target.loader_id,
                    "url": target.url,
                    "domainAndRegistry": "",
                    "securityOrigin": target.url,
                    "mimeType": "text/html",
                    "name": "",
                }}}), session)
            }
            "Page.getNavigationHistory" => {
                let Some(target) = self.targets.get(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                cdp_result_response(&request.id, json!({
                    "currentIndex": 0,
                    "entries": [{
                        "id": 1,
                        "url": target.url,
                        "userTypedURL": target.url,
                        "title": target.title,
                        "transitionType": "typed",
                    }],
                }), session)
            }
            "Page.resetNavigationHistory" => cdp_result_response(&request.id, json!({}), session),
            "DOM.getDocument" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let depth = request.params.get("depth").and_then(Value::as_i64).unwrap_or(1);
                let max_depth = if depth < 0 { 64 } else { usize::try_from(depth).unwrap_or(1).min(64) };
                let document_handle = target.document_handle;
                let document_url = target.url.clone();
                let root = match describe_node(&mut target.core, document_handle, max_depth, 0) {
                    Ok(mut root) => {
                        if let Value::Object(ref mut object) = root {
                            object.insert("documentURL".to_string(), Value::String(document_url.clone()));
                            object.insert("baseURL".to_string(), Value::String(document_url));
                        }
                        root
                    }
                    Err(error) => return cdp_error_response(&request.id, -32000, error, session),
                };
                cdp_result_response(&request.id, json!({"root": root}), session)
            }
            "DOM.querySelector" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let selector = request.params.get("selector").and_then(Value::as_str).unwrap_or("");
                let root = request.params.get("nodeId").and_then(Value::as_u64).unwrap_or(u64::from(target.document_handle));
                let result = if root == u64::from(target.document_handle) {
                    target.core.dom_op("query_selector", selector, "")
                } else {
                    target.core.dom_op("query_selector_scoped", &root.to_string(), selector)
                };
                let handle = match result {
                    Ok(value) => value.parse::<u32>().unwrap_or(0),
                    Err(_) => return cdp_error_response(&request.id, -32000, "DOM selector failed", session),
                };
                cdp_result_response(&request.id, json!({"nodeId": if handle == u32::MAX { 0 } else { handle }}), session)
            }
            "DOM.querySelectorAll" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let selector = request.params.get("selector").and_then(Value::as_str).unwrap_or("");
                let root = request.params.get("nodeId").and_then(Value::as_u64).unwrap_or(u64::from(target.document_handle));
                let result = if root == u64::from(target.document_handle) {
                    target.core.dom_op("query_selector_all", selector, "")
                } else {
                    target.core.dom_op("query_selector_all_scoped", &root.to_string(), selector)
                };
                let node_ids = match result {
                    Ok(value) => serde_json::from_str::<Vec<u32>>(&value).unwrap_or_default(),
                    Err(_) => return cdp_error_response(&request.id, -32000, "DOM selector failed", session),
                };
                cdp_result_response(&request.id, json!({"nodeIds": node_ids}), session)
            }
            "DOM.getOuterHTML" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let node_id = request.params.get("nodeId").and_then(Value::as_u64).unwrap_or(0);
                let raw = match target.core.dom_op("outer_html", &node_id.to_string(), "") {
                    Ok(value) => value,
                    Err(_) => return cdp_error_response(&request.id, -32000, "DOM node is not known", session),
                };
                let html = serde_json::from_str::<String>(&raw).unwrap_or(raw);
                cdp_result_response(&request.id, json!({"outerHTML": html}), session)
            }
            "DOM.getAttributes" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let node_id = request.params.get("nodeId").and_then(Value::as_u64).unwrap_or(0);
                let names = match target.core.dom_op("attribute_names", &node_id.to_string(), "") {
                    Ok(value) => serde_json::from_str::<Vec<String>>(&value).unwrap_or_default(),
                    Err(_) => return cdp_error_response(&request.id, -32000, "DOM node is not known", session),
                };
                let mut attributes = Vec::with_capacity(names.len().saturating_mul(2));
                for name in names {
                    let value = target
                        .core
                        .dom_op("get_attribute", &node_id.to_string(), &name)
                        .ok()
                        .and_then(|raw| serde_json::from_str::<Option<String>>(&raw).ok().flatten())
                        .unwrap_or_default();
                    attributes.push(Value::String(name));
                    attributes.push(Value::String(value));
                }
                cdp_result_response(&request.id, json!({"attributes": attributes}), session)
            }
            "DOM.describeNode" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let node_id = request.params.get("nodeId").and_then(Value::as_u64).unwrap_or(u64::from(target.document_handle));
                let depth = request.params.get("depth").and_then(Value::as_i64).unwrap_or(1);
                let max_depth = if depth < 0 { 64 } else { usize::try_from(depth).unwrap_or(1).min(64) };
                let node = match describe_node(&mut target.core, u32::try_from(node_id).unwrap_or(0), max_depth, 0) {
                    Ok(node) => node,
                    Err(error) => return cdp_error_response(&request.id, -32000, error, session),
                };
                cdp_result_response(&request.id, json!({"node": node}), session)
            }
            "DOM.requestChildNodes" => {
                let Some(target) = self.targets.get_mut(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let node_id = request.params.get("nodeId").and_then(Value::as_u64).unwrap_or(0);
                let children = match describe_children(&mut target.core, u32::try_from(node_id).unwrap_or(0), 1, 0) {
                    Ok(children) => children,
                    Err(error) => return cdp_error_response(&request.id, -32000, error, session),
                };
                if let Some(connection) = self.connections.get_mut(&connection_id) {
                    queue_event(connection, "DOM.setChildNodes", json!({"parentId": node_id, "nodes": children}), session);
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "IO.read" => {
                let Some(handle) = request.params.get("handle").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "handle is required", session);
                };
                let requested = request
                    .params
                    .get("size")
                    .and_then(Value::as_u64)
                    .unwrap_or(MAX_STREAM_CHUNK_BYTES as u64);
                if requested == 0 {
                    return cdp_error_response(&request.id, -32602, "size must be greater than zero", session);
                }
                let size = usize::try_from(requested)
                    .unwrap_or(MAX_STREAM_CHUNK_BYTES)
                    .min(MAX_STREAM_CHUNK_BYTES);
                if self.stream_connections.get(handle) != Some(&connection_id) {
                    return cdp_error_response(&request.id, -32000, "Invalid stream handle", session);
                }
                let offset = request
                    .params
                    .get("offset")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok());
                if let Some(offset) = offset {
                    if self.streams.byte_len(handle).is_none_or(|length| offset > length) {
                        return cdp_error_response(&request.id, -32602, "stream offset is out of range", session);
                    }
                }
                let Some((data, eof)) = self.streams.read(handle, offset, size) else {
                    return cdp_error_response(&request.id, -32000, "Invalid stream handle", session);
                };
                if eof {
                    self.stream_connections.remove(handle);
                    self.streams.remove(handle);
                }
                cdp_result_response(
                    &request.id,
                    json!({"base64Encoded": true, "data": data, "eof": eof}),
                    session,
                )
            }
            "IO.close" => {
                let Some(handle) = request.params.get("handle").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "handle is required", session);
                };
                if self.stream_connections.get(handle) == Some(&connection_id) {
                    self.stream_connections.remove(handle);
                    self.streams.remove(handle);
                }
                cdp_result_response(&request.id, json!({}), session)
            }
            "Page.setDocumentContent" => self.queue_action(connection_id, request.clone(), target_id, "setDocumentContent", json!({
                "html": request.params.get("html").and_then(Value::as_str).unwrap_or(""),
            })),
            "Page.navigate" => self.queue_action(connection_id, request.clone(), target_id.clone(), "navigate", json!({
                "url": request.params.get("url").and_then(Value::as_str).unwrap_or("about:blank"),
                "method": request.params.get("referrer").and_then(Value::as_str).unwrap_or("GET"),
                "extraHTTPHeaders": self.targets.get(&target_id).map(|target| target.extra_headers.clone()).unwrap_or_default(),
            })),
            "Page.reload" => {
                let url = self.targets.get(&target_id).map(|target| target.url.clone()).unwrap_or_else(|| "about:blank".to_string());
                let extra_headers = self.targets.get(&target_id).map(|target| target.extra_headers.clone()).unwrap_or_default();
                self.queue_action(connection_id, request.clone(), target_id, "reload", json!({"url": url, "extraHTTPHeaders": extra_headers}))
            }
            "Runtime.evaluate" => self.queue_action(connection_id, request.clone(), target_id, "evaluate", json!({
                "expression": request.params.get("expression").and_then(Value::as_str).unwrap_or(""),
                "returnByValue": request.params.get("returnByValue").and_then(Value::as_bool).unwrap_or(false),
            })),
            "Runtime.callFunctionOn" => self.queue_action(
                connection_id,
                request.clone(),
                target_id,
                "callFunctionOn",
                Value::Object(request.params.clone()),
            ),
            "Runtime.releaseObject" => self.queue_action(
                connection_id,
                request.clone(),
                target_id,
                "releaseObject",
                Value::Object(request.params.clone()),
            ),
            "Runtime.releaseObjectGroup" => self.queue_action(
                connection_id,
                request.clone(),
                target_id,
                "releaseObjectGroup",
                Value::Object(request.params.clone()),
            ),
            "Runtime.getProperties" => self.queue_action(
                connection_id,
                request.clone(),
                target_id,
                "getProperties",
                Value::Object(request.params.clone()),
            ),
            "Runtime.getIsolateId" => self.queue_action(
                connection_id,
                request.clone(),
                target_id,
                "getIsolateId",
                Value::Object(request.params.clone()),
            ),
            "Input.dispatchMouseEvent" => self.queue_action(
                connection_id,
                request.clone(),
                target_id,
                "dispatchMouseEvent",
                Value::Object(request.params.clone()),
            ),
            "Input.dispatchKeyEvent" => self.queue_action(
                connection_id,
                request.clone(),
                target_id,
                "dispatchKeyEvent",
                Value::Object(request.params.clone()),
            ),
            "Input.insertText" => self.queue_action(
                connection_id,
                request.clone(),
                target_id,
                "insertText",
                Value::Object(request.params.clone()),
            ),
            #[cfg(feature = "render")]
            "Page.captureScreenshot" => self.capture_screenshot(&request, target_id, session),
            #[cfg(not(feature = "render"))]
            "Page.captureScreenshot" => self.queue_action(connection_id, request.clone(), target_id, "screenshot", json!({
                "format": request.params.get("format").and_then(Value::as_str).unwrap_or("png"),
            })),
            #[cfg(feature = "render")]
            "Page.printToPDF" => self.print_to_pdf(connection_id, &request, target_id, session),
            #[cfg(not(feature = "render"))]
            "Page.printToPDF" => self.queue_action(connection_id, request.clone(), target_id, "pdf", json!({
                "landscape": request.params.get("landscape").and_then(Value::as_bool).unwrap_or(false),
            })),
            _ => cdp_error_response(&request.id, -32601, "method is not implemented", session),
        }
    }

    fn queue_action(&mut self, connection_id: u32, request: Request, target_id: String, kind: &str, payload: Value) -> Value {
        let shared_action = match self.shared_engine_action(&target_id, kind, &payload) {
            Ok(action) => action,
            Err(error) => return cdp_error_response(&request.id, -32000, error.to_string(), request.session_id.as_deref()),
        };
        let action_id = match self.action_queue.enqueue(kind, payload.clone()) {
            Ok(value) => value,
            Err(error) => {
                let message = match error {
                    obscura_cdp::action::ActionError::QueueFull => "CDP action queue is full",
                    obscura_cdp::action::ActionError::PayloadTooLarge => "CDP action payload exceeds the byte limit",
                    obscura_cdp::action::ActionError::IdExhausted => "CDP action ID space is exhausted",
                    obscura_cdp::action::ActionError::UnknownAction | obscura_cdp::action::ActionError::StaleGeneration => "CDP action queue is invalid",
                };
                return cdp_error_response(&request.id, -32000, message, request.session_id.as_deref());
            }
        };
        // The CDP response is the transport-facing action drain. Keep the
        // shared queue's pending ownership, but remove its ready copy so the
        // host cannot accidentally execute the same action twice by polling
        // both legacy and shared paths.
        let _ = self.action_queue.drain(1);
        let shared_action_id = match self.shared_state.start_action(shared_action) {
            Ok(id) => id,
            Err(error) => {
                let _ = self.action_queue.cancel(action_id);
                return cdp_error_response(&request.id, -32000, error.to_string(), request.session_id.as_deref());
            }
        };
        self.shared_actions.insert(action_id, shared_action_id);
        self.actions.insert(action_id, Action {
            connection_id,
            request_id: request.id.clone(),
            session_id: request.session_id.clone(),
            target_id: target_id.clone(),
            kind: kind.to_string(),
        });
        cdp_result_response(&request.id, json!({"obscuraAction": {
            "actionId": action_id,
            "kind": kind,
            "targetId": target_id,
            "payload": payload,
        }}), request.session_id.as_deref())
    }

    #[cfg(feature = "render")]
    fn capture_screenshot(&mut self, request: &Request, target_id: String, session: Option<&str>) -> Value {
        let format = request.params.get("format").and_then(Value::as_str).unwrap_or("png");
        if format != "png" {
            return cdp_error_response(&request.id, -32602, "portable WASM screenshots currently support only PNG", session);
        }
        let Some(target) = self.targets.get_mut(&target_id) else {
            return cdp_error_response(&request.id, -32000, "target is not known", session);
        };
        let bytes = match target
            .core
            .screenshot_png(target.viewport.width, target.viewport.height, 0.0, 0.0)
        {
            Ok(bytes) if bytes.len() <= MAX_ACTION_RESULT_BYTES => bytes,
            Ok(_) => {
                return cdp_error_response(
                    &request.id,
                    -32000,
                    "portable screenshot exceeds the response limit",
                    session,
                )
            }
            Err(_) => {
                return cdp_error_response(&request.id, -32000, "portable screenshot failed", session)
            }
        };
        cdp_result_response(
            &request.id,
            json!({"data": BASE64.encode(bytes), "fromSurface": true}),
            session,
        )
    }

    #[cfg(feature = "render")]
    fn print_to_pdf(
        &mut self,
        connection_id: u32,
        request: &Request,
        target_id: String,
        session: Option<&str>,
    ) -> Value {
        let Some(target) = self.targets.get_mut(&target_id) else {
            return cdp_error_response(&request.id, -32000, "target is not known", session);
        };
        let mut options_value = Value::Object(request.params.clone());
        if let Some(object) = options_value.as_object_mut() {
            // transferMode belongs to the CDP envelope, not the portable PDF
            // options schema, which intentionally denies unknown fields.
            object.remove("transferMode");
        }
        let options = match serde_json::to_string(&options_value) {
            Ok(options) => options,
            Err(_) => return cdp_error_response(&request.id, -32602, "invalid PDF options", session),
        };
        let bytes = match target.core.pdf(&options, target.document_handle, target.revision) {
            Ok(bytes) if bytes.len() <= MAX_ACTION_RESULT_BYTES => bytes,
            Ok(_) => {
                return cdp_error_response(
                    &request.id,
                    -32000,
                    "portable PDF exceeds the response limit",
                    session,
                )
            }
            Err(_) => return cdp_error_response(&request.id, -32000, "portable PDF failed", session),
        };
        if request.params.get("transferMode").and_then(Value::as_str) == Some("ReturnAsStream") {
            let handle = match self.streams.insert(bytes) {
                Ok(handle) => handle,
                Err(error) => return cdp_error_response(&request.id, -32000, error, session),
            };
            self.stream_connections.insert(handle.clone(), connection_id);
            return cdp_result_response(
                &request.id,
                json!({"data": "", "stream": handle}),
                session,
            );
        }
        cdp_result_response(&request.id, json!({"data": BASE64.encode(bytes)}), session)
    }

    fn shared_engine_action(
        &self,
        target_id: &str,
        kind: &str,
        payload: &Value,
    ) -> Result<EngineAction, CdpFailure> {
        let page = self
            .shared_pages
            .get(target_id)
            .copied()
            .ok_or_else(|| CdpFailure::UnknownPage(PageId::new(0)))?;
        let string_field = |name: &str, default: &str| {
            payload.get(name).and_then(Value::as_str).unwrap_or(default).to_string()
        };
        Ok(match kind {
            "navigate" | "reload" | "setDocumentContent" => EngineAction::Navigate {
                page,
                url: string_field("url", "about:blank"),
            },
            "evaluate" => EngineAction::Evaluate {
                page,
                expression: string_field("expression", ""),
            },
            "callFunctionOn" => EngineAction::CallFunctionOn {
                page,
                declaration: string_field("functionDeclaration", ""),
            },
            "getProperties" => EngineAction::GetProperties {
                page,
                object_id: string_field("objectId", ""),
            },
            "releaseObject" | "releaseObjectGroup" => EngineAction::ReleaseObject {
                page,
                object_id: string_field("objectId", ""),
            },
            "dispatchMouseEvent" | "dispatchKeyEvent" | "insertText" => EngineAction::DeliverInput {
                page,
                payload: payload.clone(),
            },
            "screenshot" => EngineAction::CaptureScreenshot {
                page,
                format: string_field("format", "png"),
            },
            "pdf" => EngineAction::PrintToPdf {
                page,
                options: payload.clone(),
            },
            "getIsolateId" => EngineAction::Wake {
                deadline_millis: 0,
            },
            _ => return Err(CdpFailure::Unsupported(format!("unknown host action kind {kind}"))),
        })
    }

    fn create_target(&mut self, context_id: &str, url: &str, html: &str) -> String {
        let shared_context = self
            .shared_contexts
            .get(context_id)
            .copied()
            .expect("portable CDP target context must be registered");
        let shared_page = self
            .shared_state
            .create_page(&shared_context, url)
            .expect("portable CDP target page must fit shared state bounds");
        self.create_target_with_shared_page(context_id, url, html, shared_page)
    }

    fn create_target_with_shared_page(
        &mut self,
        context_id: &str,
        url: &str,
        html: &str,
        shared_page: PageId,
    ) -> String {
        let page_number = u32::try_from(shared_page.get()).expect("portable page ID fits wire ID");
        self.next_target_id = self.next_target_id.max(page_number);
        let target_id = format!("page-{page_number}");
        self.shared_pages.insert(target_id.clone(), shared_page);
        let mut core = ObscuraCore::new(html).expect("empty document is valid");
        let _ = core.set_document_metadata(url, "", "UTF-8");
        let loader_id = format!("loader-{target_id}-{}", self.next_loader_id);
        self.next_loader_id = self.next_loader_id.saturating_add(1);
        self.targets.insert(target_id.clone(), Target {
            id: target_id.clone(),
            context_id: context_id.to_string(),
            frame_id: target_id.clone(),
            loader_id,
            url: url.to_string(),
            title: String::new(),
            document_handle: core.document_handle(),
            revision: core.page_revision(),
            viewport: Viewport::default(),
            emulated_media: String::new(),
            focus_emulation: false,
            cache_disabled: false,
            extra_headers: BTreeMap::new(),
            network_enabled_sessions: BTreeSet::new(),
            fetch_patterns: BTreeMap::new(),
            paused_fetches: BTreeMap::new(),
            response_bodies: BTreeMap::new(),
            core,
            sessions: BTreeSet::new(),
        });
        self.announce_target_created(&target_id);
        target_id
    }

    /// Send target discovery and automatic-attachment notifications for a
    /// newly created target. Connections and targets are BTree maps, so the
    /// order is deterministic for hosts which drain their event queues later.
    fn announce_target_created(&mut self, target_id: &str) {
        let Some(info) = self.targets.get(target_id).map(Self::target_info) else {
            return;
        };
        let connection_ids: Vec<u32> = self.connections.keys().copied().collect();
        for connection_id in connection_ids {
            let (discover, auto_attach, already_attached) = self
                .connections
                .get(&connection_id)
                .map(|connection| {
                    (
                        connection.discover_targets,
                        connection.auto_attach,
                        connection.sessions.values().any(|id| id == target_id),
                    )
                })
                .unwrap_or((false, false, true));
            if discover {
                if let Some(connection) = self.connections.get_mut(&connection_id) {
                    queue_event(connection, "Target.targetCreated", json!({"targetInfo": info}), None);
                }
            }
            if auto_attach && !already_attached {
                let session_id = self.allocate_session(connection_id, target_id);
                if let Some(connection) = self.connections.get_mut(&connection_id) {
                    queue_event(
                        connection,
                        "Target.attachedToTarget",
                        json!({"sessionId": session_id, "targetInfo": info, "waitingForDebugger": false}),
                        None,
                    );
                }
            }
        }
    }

    /// Attach every currently-live target which is not already attached on
    /// this connection. This intentionally never duplicates an existing
    /// automatic session when a client repeats `Target.setAutoAttach`.
    fn auto_attach_existing_targets(&mut self, connection_id: u32) {
        let target_ids: Vec<String> = self.targets.keys().cloned().collect();
        for target_id in target_ids {
            let attached = self
                .connections
                .get(&connection_id)
                .map(|connection| connection.sessions.values().any(|id| id == &target_id))
                .unwrap_or(true);
            if attached {
                continue;
            }
            let Some(info) = self.targets.get(&target_id).map(Self::target_info) else {
                continue;
            };
            let session_id = self.allocate_session(connection_id, &target_id);
            if let Some(connection) = self.connections.get_mut(&connection_id) {
                queue_event(
                    connection,
                    "Target.attachedToTarget",
                    json!({"sessionId": session_id, "targetInfo": info, "waitingForDebugger": false}),
                    None,
                );
            }
        }
    }

    fn allocate_session(&mut self, connection_id: u32, target_id: &str) -> String {
        self.next_session_id = self.next_session_id.saturating_add(1);
        let session_id = format!("{target_id}-session-{}", self.next_session_id);
        if let Some(connection) = self.connections.get_mut(&connection_id) {
            connection.sessions.insert(session_id.clone(), target_id.to_string());
        }
        if let Some(target) = self.targets.get_mut(target_id) {
            target.sessions.insert(session_id.clone());
        }
        if let (Some(shared_connection), Some(shared_page)) = (
            self.shared_connections.get(&connection_id).copied(),
            self.shared_pages.get(target_id).copied(),
        ) {
            if let Ok(shared_session) = self.shared_state.attach(shared_connection, shared_page) {
                self.shared_sessions.insert(session_id.clone(), shared_session);
            }
        }
        session_id
    }

    /// Invalidate one session. Any already queued page-domain event is no
    /// longer useful once the session is detached, while lifecycle events are
    /// preserved so a client observes attachment followed by detachment.
    fn detach_session(&mut self, connection_id: u32, session_id: &str) -> Option<String> {
        let target_id = self
            .connections
            .get_mut(&connection_id)
            .and_then(|connection| connection.sessions.remove(session_id))?;
        if let Some(target) = self.targets.get_mut(&target_id) {
            target.sessions.remove(session_id);
            target.network_enabled_sessions.remove(session_id);
            target.fetch_patterns.remove(session_id);
            if target.fetch_patterns.is_empty() {
                target.paused_fetches.clear();
            }
        }
        if let Some(shared_session) = self.shared_sessions.remove(session_id) {
            self.shared_state.detach(shared_session);
        }
        self.cancel_actions_where(|action| {
            action.connection_id == connection_id && action.session_id.as_deref() == Some(session_id)
        });
        if let Some(connection) = self.connections.get_mut(&connection_id) {
            discard_session_events(connection, session_id);
            queue_event(
                connection,
                "Target.detachedFromTarget",
                json!({"sessionId": session_id, "targetId": target_id}),
                None,
            );
        }
        Some(target_id)
    }

    fn destroy_target(&mut self, target_id: &str) {
        let Some(target) = self.targets.remove(target_id) else { return; };
        let shared_page = self.shared_pages.remove(target_id);
        for request_id in target.paused_fetches.keys() {
            let queued = shared_page
                .and_then(|page| {
                    self.shared_state
                        .queue_fetch_resolution(
                            &page,
                            request_id.clone(),
                            json!({
                                "requestId": request_id,
                                "action": "fail",
                                "reason": "TargetClosed",
                            }),
                        )
                        .ok()
                })
                .unwrap_or(false);
            if !queued && self.fetch_resolutions.len() < MAX_FETCH_RESOLUTIONS {
                self.fetch_resolutions.push_back(json!({
                    "requestId": request_id,
                    "action": "fail",
                    "reason": "TargetClosed",
                }));
            }
        }
        if let Some(shared_page) = shared_page {
            let pending = self
                .shared_state
                .drain_fetch_resolutions(&shared_page, MAX_FETCH_RESOLUTIONS);
            for resolution in pending {
                if self.fetch_resolutions.len() >= MAX_FETCH_RESOLUTIONS {
                    break;
                }
                self.fetch_resolutions.push_back(resolution);
            }
            let _ = self.shared_state.close_page(&shared_page);
        }
        let shared_sessions: Vec<SessionId> = target
            .sessions
            .iter()
            .filter_map(|session_id| self.shared_sessions.remove(session_id))
            .collect();
        for shared_session in shared_sessions {
            self.shared_state.detach(shared_session);
        }
        // A target close can arrive while its host operation is in flight.
        // Completing one of those actions must be rejected, not applied to a
        // subsequent page with a coincidentally similar identifier.
        self.cancel_actions_where(|action| action.target_id == target.id);
        for connection in self.connections.values_mut() {
            let doomed: Vec<String> = connection.sessions.iter().filter(|(_, id)| *id == &target.id).map(|(session, _)| session.clone()).collect();
            for session_id in doomed {
                connection.sessions.remove(&session_id);
                discard_session_events(connection, &session_id);
                queue_event(connection, "Target.detachedFromTarget", json!({"sessionId": session_id, "targetId": target.id}), None);
            }
            if connection.discover_targets {
                queue_event(connection, "Target.targetDestroyed", json!({"targetId": target.id}), None);
            }
        }
    }

    fn action_is_live(&self, action: &Action) -> bool {
        let Some(connection) = self.connections.get(&action.connection_id) else {
            return false;
        };
        let Some(session_id) = action.session_id.as_deref() else {
            return false;
        };
        connection.sessions.get(session_id).is_some_and(|target_id| target_id == &action.target_id)
            && self
                .targets
                .get(&action.target_id)
                .is_some_and(|target| target.sessions.contains(session_id))
    }

    fn cancel_actions_where<F>(&mut self, mut predicate: F)
    where
        F: FnMut(&Action) -> bool,
    {
        let ids: Vec<u32> = self
            .actions
            .iter()
            .filter_map(|(id, action)| predicate(action).then_some(*id))
            .collect();
        for id in ids {
            self.actions.remove(&id);
            let _ = self.action_queue.cancel(id);
            if let Some(shared_id) = self.shared_actions.remove(&id) {
                let _ = self.shared_state.complete_action(
                    shared_id,
                    EngineActionResult::Failed(CdpFailure::StaleAction(shared_id)),
                );
            }
        }
    }

    fn target_info(target: &Target) -> Value {
        json!({
            "targetId": target.id,
            "type": "page",
            "title": target.title,
            "url": target.url,
            "attached": !target.sessions.is_empty(),
            "openerId": Value::Null,
            "canAccessOpener": false,
            "browserContextId": target.context_id,
        })
    }
}

fn layout_metrics(target: &Target) -> Value {
    let width = target.viewport.width;
    let height = target.viewport.height;
    json!({
        "layoutViewport": {"pageX": 0, "pageY": 0, "clientWidth": width, "clientHeight": height},
        "visualViewport": {
            "offsetX": 0,
            "offsetY": 0,
            "pageX": 0,
            "pageY": 0,
            "scale": 1,
            "zoom": 1,
            "clientWidth": width,
            "clientHeight": height,
        },
        "contentSize": {"x": 0, "y": 0, "width": width, "height": height},
    })
}

fn bounded_js_string(value: &str, maximum: usize, label: &str) -> Result<(), String> {
    if value.len() > maximum {
        return Err(format!("{label} exceeds the {maximum}-byte limit"));
    }
    Ok(())
}

fn parse_fetch_patterns(params: &Map<String, Value>) -> Result<Vec<FetchPattern>, String> {
    let Some(raw) = params.get("patterns") else {
        return Ok(vec![FetchPattern { url_pattern: "*".to_string(), request_stage: "Request".to_string() }]);
    };
    let Some(patterns) = raw.as_array() else {
        return Err("Fetch patterns must be an array".to_string());
    };
    if patterns.len() > MAX_FETCH_PATTERNS {
        return Err(format!("Fetch patterns exceed the {MAX_FETCH_PATTERNS}-item limit"));
    }
    let mut parsed = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        let Some(pattern) = pattern.as_object() else {
            return Err("Fetch pattern must be an object".to_string());
        };
        let value = pattern.get("urlPattern").and_then(Value::as_str).unwrap_or("*");
        bounded_js_string(value, MAX_FETCH_PATTERN_BYTES, "Fetch urlPattern")?;
        let request_stage = pattern
            .get("requestStage")
            .and_then(Value::as_str)
            .unwrap_or("Request");
        if request_stage != "Request" && request_stage != "Response" {
            return Err("Fetch requestStage must be Request or Response".to_string());
        }
        parsed.push(FetchPattern { url_pattern: value.to_string(), request_stage: request_stage.to_string() });
    }
    if parsed.is_empty() {
        parsed.push(FetchPattern { url_pattern: "*".to_string(), request_stage: "Request".to_string() });
    }
    Ok(parsed)
}

fn validate_fetch_header_array(value: &Value) -> Result<(), String> {
    let Some(headers) = value.as_array() else {
        return Err("Fetch headers must be an array".to_string());
    };
    if headers.len() > 256 {
        return Err("Fetch headers exceed the 256-item limit".to_string());
    }
    for header in headers {
        let Some(header) = header.as_object() else {
            return Err("Fetch header must be an object".to_string());
        };
        let name = header.get("name").and_then(Value::as_str).unwrap_or("");
        let value = header.get("value").and_then(Value::as_str).unwrap_or("");
        if name.is_empty() {
            return Err("Fetch header name is required".to_string());
        }
        bounded_js_string(name, 1024, "Fetch header name")?;
        bounded_js_string(value, MAX_NAVIGATION_HEADERS_BYTES, "Fetch header value")?;
    }
    Ok(())
}

const MAX_CDP_COOKIE_COUNT: usize = 4096;
const MAX_CDP_COOKIE_BYTES: usize = 64 * 1024;
const MAX_EXTRA_HEADER_COUNT: usize = 128;
const MAX_EXTRA_HEADER_BYTES: usize = 128 * 1024;

fn cookie_clock(params: &Map<String, Value>) -> Result<u64, String> {
    match params.get("_obscuraNowSecs") {
        None => Ok(0),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| "_obscuraNowSecs must be a non-negative integer".to_string()),
    }
}

fn cookie_url_host(value: &str) -> Option<String> {
    Url::parse(value)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
}

fn cdp_cookie_import(params: &Map<String, Value>, target_url: &str) -> Result<String, String> {
    let Some(values) = params.get("cookies").and_then(Value::as_array) else {
        return Err("cookies must be an array".to_string());
    };
    if values.len() > MAX_CDP_COOKIE_COUNT {
        return Err("cookies exceeds the 4096-cookie limit".to_string());
    }
    let target_host = cookie_url_host(target_url);
    let mut normalized = Vec::with_capacity(values.len());
    for (index, cookie) in values.iter().enumerate() {
        let Some(cookie) = cookie.as_object() else {
            return Err(format!("cookies[{index}] must be an object"));
        };
        let name = cookie.get("name").and_then(Value::as_str).unwrap_or("");
        let value = cookie.get("value").and_then(Value::as_str).unwrap_or("");
        if name.is_empty() {
            return Err(format!("cookies[{index}].name is required"));
        }
        let url_host = cookie
            .get("url")
            .and_then(Value::as_str)
            .and_then(cookie_url_host);
        let domain = cookie
            .get("domain")
            .and_then(Value::as_str)
            .map(|domain| domain.trim_start_matches('.').to_ascii_lowercase())
            .filter(|domain| !domain.is_empty())
            .or(url_host)
            .or_else(|| target_host.clone())
            .ok_or_else(|| format!("cookies[{index}] requires a domain or URL"))?;
        let path = cookie
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| path.starts_with('/'))
            .unwrap_or("/");
        let same_site = match cookie.get("sameSite").and_then(Value::as_str) {
            Some("Strict") => "Strict",
            Some("None") => "None",
            _ => "Lax",
        };
        let expires = cookie.get("expires").and_then(|value| {
            let value = value.as_f64()?;
            if !value.is_finite() || value < 0.0 || value > i64::MAX as f64 {
                None
            } else {
                Some(value as i64)
            }
        });
        let normalized_cookie = json!({
            "name": name,
            "value": value,
            "domain": domain,
            "path": path,
            "secure": cookie.get("secure").and_then(Value::as_bool).unwrap_or(false),
            "httpOnly": cookie.get("httpOnly").and_then(Value::as_bool).unwrap_or(false),
            "sameSite": same_site,
            "expires": expires,
        });
        normalized.push(normalized_cookie);
    }
    let encoded = serde_json::to_string(&normalized).map_err(|error| error.to_string())?;
    if encoded.len() > MAX_CDP_COOKIE_BYTES {
        return Err("cookies exceeds the 64KiB limit".to_string());
    }
    Ok(encoded)
}

fn parse_extra_headers(params: &Map<String, Value>) -> Result<BTreeMap<String, String>, String> {
    let Some(values) = params.get("headers").and_then(Value::as_object) else {
        return Err("headers must be an object".to_string());
    };
    if values.len() > MAX_EXTRA_HEADER_COUNT {
        return Err(format!("headers exceeds the {MAX_EXTRA_HEADER_COUNT}-header limit"));
    }
    let mut total = 0usize;
    let mut headers = BTreeMap::new();
    for (name, value) in values {
        if name.is_empty() || name.len() > 1024 || !name.bytes().all(is_header_name_byte) {
            return Err(format!("invalid HTTP header name {name:?}"));
        }
        let Some(value) = value.as_str() else {
            return Err(format!("HTTP header {name:?} must be a string"));
        };
        if value.len() > MAX_EXTRA_HEADER_BYTES || value.bytes().any(|byte| byte == b'\r' || byte == b'\n') {
            return Err(format!("invalid HTTP header value for {name:?}"));
        }
        total = total.saturating_add(name.len()).saturating_add(value.len());
        if total > MAX_EXTRA_HEADER_BYTES {
            return Err(format!("headers exceeds the {MAX_EXTRA_HEADER_BYTES}-byte limit"));
        }
        headers.insert(name.clone(), value.to_string());
    }
    Ok(headers)
}

fn is_header_name_byte(byte: u8) -> bool {
    matches!(byte,
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' |
        b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' |
        b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
    )
}

const MAX_DESCRIBED_NODES: usize = 4096;

fn describe_children(
    core: &mut ObscuraCore,
    node_id: u32,
    depth: usize,
    seen: usize,
) -> Result<Vec<Value>, String> {
    if seen >= MAX_DESCRIBED_NODES {
        return Err("DOM description exceeds the node limit".to_string());
    }
    let raw = core
        .dom_op("child_nodes", &node_id.to_string(), "")
        .map_err(|_| "DOM node is not known".to_string())?;
    let ids = serde_json::from_str::<Vec<u32>>(&raw)
        .map_err(|_| "DOM child node response is invalid".to_string())?;
    ids.into_iter()
        .enumerate()
        .map(|(index, child)| describe_node(core, child, depth, seen.saturating_add(index)))
        .collect()
}

fn describe_node(
    core: &mut ObscuraCore,
    node_id: u32,
    depth: usize,
    seen: usize,
) -> Result<Value, String> {
    if node_id == 0 || seen >= MAX_DESCRIBED_NODES {
        return Err("DOM node description limit exceeded".to_string());
    }
    let handle = node_id.to_string();
    let node_type = core
        .dom_op("node_type", &handle, "")
        .map_err(|_| "DOM node is not known".to_string())?
        .parse::<u32>()
        .unwrap_or(0);
    if node_type == 0 {
        return Err("DOM node is not known".to_string());
    }
    let node_name_command = if node_type == 1 { "tag_name" } else { "node_name" };
    let node_name = core
        .dom_op(node_name_command, &handle, "")
        .map_err(|_| "DOM node name is unavailable".to_string())
        .and_then(|value| serde_json::from_str::<String>(&value).map_err(|_| "DOM node name is invalid".to_string()))?;
    let local_name = core
        .dom_op("local_name", &handle, "")
        .ok()
        .and_then(|value| serde_json::from_str::<String>(&value).ok())
        .unwrap_or_default();
    let node_value = if matches!(node_type, 3 | 7 | 8) {
        core.dom_op("text_content", &handle, "")
            .ok()
            .and_then(|value| serde_json::from_str::<String>(&value).ok())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let mut node = json!({
        "nodeId": node_id,
        "backendNodeId": node_id,
        "nodeType": node_type,
        "nodeName": node_name,
        "localName": local_name,
        "nodeValue": node_value,
        "childNodeCount": 0,
    });
    if node_type == 1 {
        let attributes = core
            .dom_op("attribute_names", &handle, "")
            .ok()
            .and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
            .unwrap_or_default();
        let mut flat = Vec::with_capacity(attributes.len().saturating_mul(2));
        for name in attributes {
            let value = core
                .dom_op("get_attribute", &handle, &name)
                .ok()
                .and_then(|value| serde_json::from_str::<Option<String>>(&value).ok().flatten())
                .unwrap_or_default();
            flat.push(Value::String(name));
            flat.push(Value::String(value));
        }
        if let Value::Object(ref mut object) = node {
            object.insert("attributes".to_string(), Value::Array(flat));
        }
    }
    let child_count = core
        .dom_op("child_nodes", &handle, "")
        .ok()
        .and_then(|value| serde_json::from_str::<Vec<u32>>(&value).ok())
        .map(|children| children.len())
        .unwrap_or(0);
    if let Value::Object(ref mut object) = node {
        object.insert("childNodeCount".to_string(), json!(child_count));
    }
    if depth > 0 {
        let children = describe_children(core, node_id, depth.saturating_sub(1), seen.saturating_add(1))?;
        if let Value::Object(ref mut object) = node {
            object.insert("childNodeCount".to_string(), json!(children.len()));
            object.insert("children".to_string(), Value::Array(children));
        }
    }
    Ok(node)
}

fn queue_event(connection: &mut Connection, method: &str, params: Value, session_id: Option<&str>) {
    if connection.events.len() >= MAX_EVENT_QUEUE {
        connection.events.pop_front();
    }
    let mut event = Map::new();
    event.insert("method".to_string(), Value::String(method.to_string()));
    event.insert("params".to_string(), params);
    if let Some(session_id) = session_id {
        event.insert("sessionId".to_string(), Value::String(session_id.to_string()));
    }
    connection.events.push_back(Value::Object(event));
}

/// Remove queued page-domain events for a session which has just been
/// invalidated. Target lifecycle notifications intentionally do not carry a
/// top-level `sessionId`, so they remain in order around the detach event.
fn discard_session_events(connection: &mut Connection, session_id: &str) {
    connection.events.retain(|event| {
        event
            .get("sessionId")
            .and_then(Value::as_str)
            .is_none_or(|queued_session| queued_session != session_id)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(value: &str) -> Value {
        serde_json::from_str(value).expect("valid JSON returned by the portable ABI")
    }

    #[test]
    fn browser_connection_and_target_attachment_are_stateful() {
        let mut cdp = PortableCdp::new("<html><body><h1>Portable</h1></body></html>").unwrap();
        let connection = cdp.open_connection().unwrap();
        let version = json(&cdp.cdp_request(connection, r#"{"id":1,"method":"Browser.getVersion"}"#).unwrap());
        assert_eq!(version["result"]["product"], "Obscura/WASM");

        let targets = json(&cdp.cdp_request(connection, r#"{"id":2,"method":"Target.getTargets"}"#).unwrap());
        assert_eq!(targets["result"]["targetInfos"].as_array().unwrap().len(), 1);
        assert_eq!(targets["result"]["targetInfos"][0]["canAccessOpener"], false);

        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":3,"method":"Target.attachToTarget","params":{"targetId":"page-1","flatten":true}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap().to_string();
        let events = json(&cdp.poll_cdp_events(connection, 8).unwrap());
        assert_eq!(events.as_array().unwrap().len(), 1);
        assert_eq!(events[0]["method"], "Target.attachedToTarget");
        assert_eq!(events[0]["params"]["sessionId"], session);
    }

    #[test]
    fn host_actions_preserve_request_identity_until_completion() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1","flatten":true}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap();
        let request = format!(r#"{{"id":9,"sessionId":"{session}","method":"Runtime.evaluate","params":{{"expression":"6*7","returnByValue":true}}}}"#);
        let queued = json(&cdp.cdp_request(connection, &request).unwrap());
        let action_id = queued["result"]["obscuraAction"]["actionId"].as_u64().unwrap() as u32;
        assert_eq!(queued["result"]["obscuraAction"]["kind"], "evaluate");
        let completed = json(&cdp.complete_action(action_id, r#"{"result":{"type":"number","value":42}}"#).unwrap());
        assert_eq!(completed["id"], 9);
        assert_eq!(completed["sessionId"], session);
        assert_eq!(completed["result"]["result"]["value"], 42);
        assert!(cdp.complete_action(action_id, r#"{}"#).is_err());
    }

    #[test]
    fn host_action_errors_remain_cdp_errors() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1"}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap();
        let request = format!(r#"{{"id":2,"sessionId":"{session}","method":"Runtime.evaluate","params":{{"expression":"1"}}}}"#);
        let queued = json(&cdp.cdp_request(connection, &request).unwrap());
        let action_id = queued["result"]["obscuraAction"]["actionId"].as_u64().unwrap() as u32;
        let failed = json(&cdp.complete_action(
            action_id,
            r#"{"error":{"code":-32001,"message":"host timeout","data":{"retryable":true}}}"#,
        ).unwrap());
        assert_eq!(failed["id"], 2);
        assert_eq!(failed["sessionId"], session);
        assert_eq!(failed["error"]["code"], -32001);
        assert_eq!(failed["error"]["message"], "host timeout");
        assert_eq!(failed["error"]["data"]["retryable"], true);
    }

    #[test]
    fn portable_dom_commands_read_the_wasm_document_tree() {
        let mut cdp = PortableCdp::new(
            "<!doctype html><html><body><h1 id=title>Portable</h1></body></html>",
        )
        .unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1"}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap().to_string();
        cdp.poll_cdp_events(connection, 8).unwrap();

        let document = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":2,"sessionId":"{session}","method":"DOM.getDocument","params":{{"depth":-1}}}}"#),
        ).unwrap());
        assert_eq!(document["result"]["root"]["nodeType"], 9);
        assert_eq!(document["result"]["root"]["children"][0]["nodeName"], "html");
        assert!(document["result"]["root"]["children"][0]["children"].as_array().is_some());

        let selected = json(&cdp.cdp_request(
            connection,
            &format!(r##"{{"id":3,"sessionId":"{session}","method":"DOM.querySelector","params":{{"nodeId":{},"selector":"#title"}}}}"##, cdp.targets["page-1"].document_handle),
        ).unwrap());
        let node_id = selected["result"]["nodeId"].as_u64().unwrap();
        assert!(node_id > 0);
        let html = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":4,"sessionId":"{session}","method":"DOM.getOuterHTML","params":{{"nodeId":{node_id}}}}}"#),
        ).unwrap());
        assert_eq!(html["result"]["outerHTML"], "<h1 id=\"title\">Portable</h1>");

        cdp.cdp_request(
            connection,
            &format!(r#"{{"id":5,"sessionId":"{session}","method":"DOM.requestChildNodes","params":{{"nodeId":{node_id}}}}}"#),
        )
        .unwrap();
        let events = json(&cdp.poll_cdp_events(connection, 8).unwrap());
        assert_eq!(events[0]["method"], "DOM.setChildNodes");
        assert_eq!(events[0]["params"]["parentId"], node_id);
    }

    #[test]
    fn portable_cookie_commands_use_the_wasm_cookie_jar() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
                connection,
                r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1"}}"#,
            )
            .unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap();
        let set = format!(
            r#"{{"id":2,"sessionId":"{session}","method":"Network.setCookies","params":{{"_obscuraNowSecs":100,"cookies":[{{"name":"sid","value":"abc","domain":"example.test","path":"/","httpOnly":true}}]}}}}"#
        );
        assert_eq!(json(&cdp.cdp_request(connection, &set).unwrap())["result"], json!({}));

        let get = format!(
            r#"{{"id":3,"sessionId":"{session}","method":"Network.getAllCookies","params":{{"_obscuraNowSecs":100}}}}"#
        );
        let cookies = json(&cdp.cdp_request(connection, &get).unwrap());
        assert_eq!(cookies["result"]["cookies"][0]["name"], "sid");
        assert_eq!(cookies["result"]["cookies"][0]["value"], "abc");

        let delete = format!(
            r#"{{"id":4,"sessionId":"{session}","method":"Network.deleteCookies","params":{{"name":"sid","domain":"example.test","path":"/"}}}}"#
        );
        assert_eq!(json(&cdp.cdp_request(connection, &delete).unwrap())["result"], json!({}));
        let empty = json(&cdp.cdp_request(connection, &get).unwrap());
        assert!(empty["result"]["cookies"].as_array().unwrap().is_empty());
    }

    #[test]
    fn portable_io_streams_are_bounded_and_owned_by_wasm() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1"}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap();
        let handle = cdp.open_stream(connection, &BASE64.encode(b"portable-stream-data")).unwrap();
        let first = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":2,"sessionId":"{session}","method":"IO.read","params":{{"handle":"{handle}","size":8}}}}"#),
        ).unwrap());
        assert_eq!(first["result"]["base64Encoded"], true);
        assert_eq!(BASE64.decode(first["result"]["data"].as_str().unwrap()).unwrap(), b"portable");
        assert_eq!(first["result"]["eof"], false);

        let second = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":3,"sessionId":"{session}","method":"IO.read","params":{{"handle":"{handle}"}}}}"#),
        ).unwrap());
        assert_eq!(BASE64.decode(second["result"]["data"].as_str().unwrap()).unwrap(), b"-stream-data");
        assert_eq!(second["result"]["eof"], true);

        let closed = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":4,"sessionId":"{session}","method":"IO.close","params":{{"handle":"{handle}"}}}}"#),
        ).unwrap());
        assert_eq!(closed["result"], json!({}));
    }

    #[test]
    fn portable_network_events_and_response_bodies_are_owned_by_wasm() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1","flatten":true}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap().to_string();
        cdp.poll_cdp_events(connection, 8).unwrap();
        let enable = format!(r#"{{"id":2,"sessionId":"{session}","method":"Network.enable"}}"#);
        assert_eq!(json(&cdp.cdp_request(connection, &enable).unwrap())["result"], json!({}));
        let navigate = format!(r#"{{"id":3,"sessionId":"{session}","method":"Page.navigate","params":{{"url":"https://example.test/"}}}}"#);
        let queued = json(&cdp.cdp_request(connection, &navigate).unwrap());
        let action_id = queued["result"]["obscuraAction"]["actionId"].as_u64().unwrap() as u32;
        let body = BASE64.encode(b"<html>network</html>");
        let completed = json(&cdp.complete_action(action_id, &format!(
            r#"{{"url":"https://example.test/","loaderId":"loader-1","__obscuraNetwork":[{{"requestId":"loader-1","loaderId":"loader-1","url":"https://example.test/","method":"GET","requestHeaders":{{"x-test":"yes"}},"status":200,"responseHeaders":{{"content-type":"text/html"}},"mimeType":"text/html","bodyBase64":"{body}","bodySize":20,"timestamp":1.0,"wallTime":1.0}}]}}"#
        )).unwrap());
        assert_eq!(completed["result"]["url"], "https://example.test/");
        let events = json(&cdp.poll_cdp_events(connection, 8).unwrap());
        assert_eq!(events.as_array().unwrap().iter().filter(|event| event["method"] == "Network.requestWillBeSent").count(), 1);
        assert_eq!(events.as_array().unwrap().iter().filter(|event| event["method"] == "Network.responseReceived").count(), 1);
        assert_eq!(events.as_array().unwrap().iter().filter(|event| event["method"] == "Network.loadingFinished").count(), 1);
        let get_body = format!(r#"{{"id":4,"sessionId":"{session}","method":"Network.getResponseBody","params":{{"requestId":"loader-1"}}}}"#);
        let response = json(&cdp.cdp_request(connection, &get_body).unwrap());
        assert_eq!(response["result"]["base64Encoded"], true);
        assert_eq!(BASE64.decode(response["result"]["body"].as_str().unwrap()).unwrap(), b"<html>network</html>");
        let disable = format!(r#"{{"id":5,"sessionId":"{session}","method":"Network.disable"}}"#);
        assert_eq!(json(&cdp.cdp_request(connection, &disable).unwrap())["result"], json!({}));
        assert_eq!(json(&cdp.cdp_request(connection, &get_body).unwrap())["error"]["code"], -32000);
    }

    #[test]
    fn asynchronous_network_metadata_enters_the_same_wasm_event_queue() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1","flatten":true}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap().to_string();
        cdp.poll_cdp_events(connection, 8).unwrap();
        let enable = format!(r#"{{"id":2,"sessionId":"{session}","method":"Network.enable"}}"#);
        assert_eq!(json(&cdp.cdp_request(connection, &enable).unwrap())["result"], json!({}));
        let body = BASE64.encode(b"fetch-body");
        cdp.record_network_metadata_json(
            "page-1",
            &format!(r#"[{{"requestId":"fetch-1","loaderId":"loader-1","url":"https://example.test/api","method":"GET","requestHeaders":{{}},"status":200,"responseHeaders":{{"content-type":"text/plain"}},"mimeType":"text/plain","bodyBase64":"{body}","bodySize":10,"timestamp":2.0,"wallTime":2.0,"resourceType":"Fetch","initiatorType":"script"}}]"#),
        ).unwrap();
        let events = json(&cdp.poll_cdp_events(connection, 8).unwrap());
        assert_eq!(events.as_array().unwrap().iter().filter(|event| event["method"] == "Network.requestWillBeSent").count(), 1);
        let get_body = format!(r#"{{"id":3,"sessionId":"{session}","method":"Network.getResponseBody","params":{{"requestId":"fetch-1"}}}}"#);
        let response = json(&cdp.cdp_request(connection, &get_body).unwrap());
        assert_eq!(BASE64.decode(response["result"]["body"].as_str().unwrap()).unwrap(), b"fetch-body");
    }

    #[test]
    fn portable_fetch_interception_pauses_and_drains_host_resolution() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1","flatten":true}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap().to_string();
        cdp.poll_cdp_events(connection, 8).unwrap();
        let enabled = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":2,"sessionId":"{session}","method":"Fetch.enable","params":{{"patterns":[{{"urlPattern":"https://example.test/api/*"}}]}}}}"#),
        ).unwrap());
        assert_eq!(enabled["result"], json!({}));

        let paused = json(&cdp.intercept_fetch_request_json(
            "page-1",
            r#"{"requestId":"fetch-1","url":"https://example.test/api/data","method":"GET","headers":{"x-test":"yes"},"resourceType":"Fetch","frameId":"page-1"}"#,
        ).unwrap());
        assert_eq!(paused["paused"], true);
        let events = json(&cdp.poll_cdp_events(connection, 8).unwrap());
        assert_eq!(events[0]["method"], "Fetch.requestPaused");
        assert_eq!(events[0]["params"]["requestId"], "fetch-1");
        assert_eq!(events[0]["params"]["request"]["headers"]["x-test"], "yes");

        let continued = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":3,"sessionId":"{session}","method":"Fetch.continueRequest","params":{{"requestId":"fetch-1","url":"https://example.test/api/rewritten","method":"POST","postData":"body","headers":[{{"name":"x-rewritten","value":"ok"}}]}}}}"#),
        ).unwrap());
        assert_eq!(continued["result"], json!({}));
        let resolutions = json(&cdp.drain_fetch_resolutions_json().unwrap());
        assert_eq!(resolutions[0]["action"], "continue");
        assert_eq!(resolutions[0]["url"], "https://example.test/api/rewritten");
        assert_eq!(resolutions[0]["method"], "POST");
        assert_eq!(resolutions[0]["headers"][0]["name"], "x-rewritten");
        assert_eq!(resolutions[0]["postData"], "body");

        let paused_again = json(&cdp.intercept_fetch_request_json(
            "page-1",
            r#"{"requestId":"fetch-2","url":"https://example.test/api/again","method":"GET","headers":{},"resourceType":"Fetch","frameId":"page-1"}"#,
        ).unwrap());
        assert_eq!(paused_again["paused"], true);
        let fulfilled = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":4,"sessionId":"{session}","method":"Fetch.fulfillRequest","params":{{"requestId":"fetch-2","responseCode":201,"responseHeaders":[{{"name":"content-type","value":"text/plain"}}],"body":"{}"}}}}"#, BASE64.encode(b"fulfilled")),
        ).unwrap());
        assert_eq!(fulfilled["result"], json!({}));
        let resolutions = json(&cdp.drain_fetch_resolutions_json().unwrap());
        assert_eq!(resolutions[0]["action"], "fulfill");
        assert_eq!(resolutions[0]["status"], 201);
        assert_eq!(BASE64.decode(resolutions[0]["bodyBase64"].as_str().unwrap()).unwrap(), b"fulfilled");
    }

    #[test]
    fn portable_fetch_response_stage_exposes_bounded_body() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1","flatten":true}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap().to_string();
        cdp.poll_cdp_events(connection, 8).unwrap();
        let enabled = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":2,"sessionId":"{session}","method":"Fetch.enable","params":{{"patterns":[{{"urlPattern":"https://example.test/*","requestStage":"Response"}}]}}}}"#),
        ).unwrap());
        assert_eq!(enabled["result"], json!({}));
        let body = BASE64.encode(b"response-body");
        let paused = json(&cdp.intercept_fetch_request_json(
            "page-1",
            &format!(r#"{{"requestId":"fetch-response-1","url":"https://example.test/api","method":"GET","headers":{{}},"resourceType":"Fetch","frameId":"page-1","requestStage":"Response","responseStatusCode":201,"responseHeaders":{{"content-type":"text/plain"}},"responseBodyBase64":"{body}"}}"#),
        ).unwrap());
        assert_eq!(paused["paused"], true);
        let event = json(&cdp.poll_cdp_events(connection, 8).unwrap());
        assert_eq!(event[0]["params"]["responseStatusCode"], 201);
        assert_eq!(event[0]["params"]["responseHeaders"][0]["name"], "content-type");
        let get_body = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":3,"sessionId":"{session}","method":"Fetch.getResponseBody","params":{{"requestId":"fetch-response-1"}}}}"#),
        ).unwrap());
        assert_eq!(BASE64.decode(get_body["result"]["body"].as_str().unwrap()).unwrap(), b"response-body");
        let continued = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":4,"sessionId":"{session}","method":"Fetch.continueRequest","params":{{"requestId":"fetch-response-1"}}}}"#),
        ).unwrap());
        assert_eq!(continued["result"], json!({}));
        assert_eq!(json(&cdp.drain_fetch_resolutions_json().unwrap())[0]["action"], "continue");
    }

    #[test]
    fn portable_fetch_target_close_drains_shared_failure_resolution() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1","flatten":true}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap().to_string();
        let enabled = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":2,"sessionId":"{session}","method":"Fetch.enable"}}"#),
        ).unwrap());
        assert_eq!(enabled["result"], json!({}));
        let paused = json(&cdp.intercept_fetch_request_json(
            "page-1",
            r#"{"requestId":"close-me","url":"https://example.test/close","method":"GET","headers":{},"resourceType":"Fetch","frameId":"page-1"}"#,
        ).unwrap());
        assert_eq!(paused["paused"], true);
        assert_eq!(json(&cdp.cdp_request(
            connection,
            r#"{"id":3,"method":"Target.closeTarget","params":{"targetId":"page-1"}}"#,
        ).unwrap())["result"]["success"], true);
        let resolutions = json(&cdp.drain_fetch_resolutions_json().unwrap());
        assert_eq!(resolutions[0]["action"], "fail");
        assert_eq!(resolutions[0]["reason"], "TargetClosed");
    }

    #[test]
    fn portable_cache_policy_is_owned_by_target_state() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1","flatten":true}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap();
        cdp.poll_cdp_events(connection, 8).unwrap();
        assert_eq!(cdp.cache_disabled_json("page-1").unwrap(), false);
        let enabled = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":2,"sessionId":"{session}","method":"Network.setCacheDisabled","params":{{"cacheDisabled":true}}}}"#),
        ).unwrap());
        assert_eq!(enabled["result"], json!({}));
        assert_eq!(cdp.cache_disabled_json("page-1").unwrap(), true);
        let cleared = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":3,"sessionId":"{session}","method":"Network.clearBrowserCache"}}"#),
        ).unwrap());
        assert_eq!(cleared["result"], json!({}));
        let disabled = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":4,"sessionId":"{session}","method":"Network.setCacheDisabled","params":{{"cacheDisabled":false}}}}"#),
        ).unwrap());
        assert_eq!(disabled["result"], json!({}));
        assert_eq!(cdp.cache_disabled_json("page-1").unwrap(), false);
    }

    #[test]
    fn portable_emulation_state_drives_layout_metrics() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1"}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap();
        let set = format!(
            r#"{{"id":2,"sessionId":"{session}","method":"Emulation.setDeviceMetricsOverride","params":{{"width":640,"height":480,"deviceScaleFactor":2}}}}"#
        );
        let set_result = json(&cdp.cdp_request(connection, &set).unwrap());
        assert_eq!(set_result["result"], json!({}));
        let metrics = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":3,"sessionId":"{session}","method":"Page.getLayoutMetrics"}}"#),
        ).unwrap());
        assert_eq!(metrics["result"]["layoutViewport"]["clientWidth"], 640);
        assert_eq!(metrics["result"]["layoutViewport"]["clientHeight"], 480);
        let invalid = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":4,"sessionId":"{session}","method":"Emulation.setDeviceMetricsOverride","params":{{"width":0,"height":480}}}}"#),
        ).unwrap());
        assert_eq!(invalid["error"]["code"], -32602);
    }

    #[test]
    fn portable_history_is_stateful_and_reload_stays_host_bounded() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1"}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap();
        let history = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":2,"sessionId":"{session}","method":"Page.getNavigationHistory"}}"#),
        ).unwrap());
        assert_eq!(history["result"]["currentIndex"], 0);
        assert_eq!(history["result"]["entries"].as_array().unwrap().len(), 1);
        let reload = json(&cdp.cdp_request(
            connection,
            &format!(r#"{{"id":3,"sessionId":"{session}","method":"Page.reload"}}"#),
        ).unwrap());
        assert_eq!(reload["result"]["obscuraAction"]["kind"], "reload");
    }

    #[test]
    fn portable_runtime_remote_object_commands_are_host_actions() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1"}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap();
        for (id, method, kind) in [
            (2, "Runtime.callFunctionOn", "callFunctionOn"),
            (3, "Runtime.getProperties", "getProperties"),
            (4, "Runtime.releaseObject", "releaseObject"),
            (5, "Runtime.releaseObjectGroup", "releaseObjectGroup"),
            (6, "Runtime.getIsolateId", "getIsolateId"),
        ] {
            let request = format!(
                r#"{{"id":{id},"sessionId":"{session}","method":"{method}","params":{{}}}}"#
            );
            let queued = json(&cdp.cdp_request(connection, &request).unwrap());
            assert_eq!(queued["result"]["obscuraAction"]["kind"], kind);
        }
        let headers = format!(
            r#"{{"id":7,"sessionId":"{session}","method":"Network.setExtraHTTPHeaders","params":{{"headers":{{"X-Portable":"wasm"}}}}}}"#
        );
        assert_eq!(json(&cdp.cdp_request(connection, &headers).unwrap())["result"], json!({}));
        let navigate = format!(
            r#"{{"id":8,"sessionId":"{session}","method":"Page.navigate","params":{{"url":"https://example.test/"}}}}"#
        );
        let queued = json(&cdp.cdp_request(connection, &navigate).unwrap());
        assert_eq!(queued["result"]["obscuraAction"]["payload"]["extraHTTPHeaders"]["X-Portable"], "wasm");
    }

    #[test]
    fn malformed_messages_and_unknown_sessions_are_cdp_errors() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let malformed = json(&cdp.cdp_request(connection, "not-json").unwrap());
        assert_eq!(malformed["error"]["code"], -32700);
        let unknown = json(&cdp.cdp_request(
            connection,
            r#"{"id":4,"sessionId":"missing","method":"Page.enable"}"#,
        ).unwrap());
        assert_eq!(unknown["id"], 4);
        assert_eq!(unknown["error"]["code"], -32000);
    }

    #[test]
    fn closing_a_connection_drops_its_actions_and_events() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1"}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap();
        let request = format!(r#"{{"id":2,"sessionId":"{session}","method":"Page.navigate","params":{{"url":"https://example.test/"}}}}"#);
        let queued = json(&cdp.cdp_request(connection, &request).unwrap());
        let action_id = queued["result"]["obscuraAction"]["actionId"].as_u64().unwrap() as u32;
        cdp.close_connection(connection).unwrap();
        assert!(cdp.complete_action(action_id, r#"{"url":"https://example.test/"}"#).is_err());
        let closed = json(&cdp.cdp_request(connection, r#"{"id":3,"method":"Browser.getVersion"}"#).unwrap());
        assert_eq!(closed["error"]["code"], -32000);
    }

    #[test]
    fn auto_attach_existing_targets_is_ordered_and_idempotent() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();

        let enabled = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.setAutoAttach","params":{"autoAttach":true,"flatten":true}}"#,
        ).unwrap());
        assert_eq!(enabled["result"], json!({}));
        let events = json(&cdp.poll_cdp_events(connection, 8).unwrap());
        assert_eq!(events.as_array().unwrap().len(), 1);
        assert_eq!(events[0]["method"], "Target.attachedToTarget");
        assert_eq!(events[0]["params"]["targetInfo"]["targetId"], "page-1");

        let repeated = json(&cdp.cdp_request(
            connection,
            r#"{"id":2,"method":"Target.setAutoAttach","params":{"autoAttach":true,"flatten":true}}"#,
        ).unwrap());
        assert_eq!(repeated["result"], json!({}));
        let events = json(&cdp.poll_cdp_events(connection, 8).unwrap());
        assert!(events.as_array().unwrap().is_empty());
    }

    #[test]
    fn created_targets_emit_discovery_then_auto_attach() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.setDiscoverTargets","params":{"discover":true}}"#,
        ).unwrap();
        // Discovery emits a snapshot of the initial page. The test below is
        // about lifecycle order for the subsequently-created target.
        cdp.poll_cdp_events(connection, 8).unwrap();
        cdp.cdp_request(
            connection,
            r#"{"id":2,"method":"Target.setAutoAttach","params":{"autoAttach":true,"flatten":true}}"#,
        ).unwrap();
        cdp.poll_cdp_events(connection, 8).unwrap();

        let created = json(&cdp.cdp_request(
            connection,
            r#"{"id":3,"method":"Target.createTarget","params":{"url":"https://example.test/new"}}"#,
        ).unwrap());
        let target_id = created["result"]["targetId"].as_str().unwrap().to_string();
        let events = json(&cdp.poll_cdp_events(connection, 8).unwrap());
        assert_eq!(events.as_array().unwrap().len(), 2);
        assert_eq!(events[0]["method"], "Target.targetCreated");
        assert_eq!(events[0]["params"]["targetInfo"]["targetId"], target_id);
        assert_eq!(events[1]["method"], "Target.attachedToTarget");
        assert_eq!(events[1]["params"]["targetInfo"]["targetId"], target_id);
        assert!(events[1]["params"]["sessionId"].as_str().is_some());
    }

    #[test]
    fn detach_and_close_invalidate_actions_and_page_events() {
        let mut cdp = PortableCdp::new("").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp.cdp_request(
            connection,
            r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1","flatten":true}}"#,
        ).unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap().to_string();
        cdp.poll_cdp_events(connection, 8).unwrap();

        let enable = format!(r#"{{"id":2,"sessionId":"{session}","method":"Runtime.enable"}}"#);
        cdp.cdp_request(connection, &enable).unwrap();
        let navigate = format!(r#"{{"id":3,"sessionId":"{session}","method":"Page.navigate","params":{{"url":"https://example.test/"}}}}"#);
        let queued = json(&cdp.cdp_request(connection, &navigate).unwrap());
        let action_id = queued["result"]["obscuraAction"]["actionId"].as_u64().unwrap() as u32;

        let detach = format!(r#"{{"id":4,"method":"Target.detachFromTarget","params":{{"sessionId":"{session}"}}}}"#);
        assert_eq!(json(&cdp.cdp_request(connection, &detach).unwrap())["result"], json!({}));
        assert!(cdp.complete_action(action_id, r#"{"url":"https://example.test/"}"#).is_err());
        let events = json(&cdp.poll_cdp_events(connection, 8).unwrap());
        assert_eq!(events.as_array().unwrap().len(), 1);
        assert_eq!(events[0]["method"], "Target.detachedFromTarget");
        assert_eq!(events[0]["params"]["sessionId"], session);

        // A closed target likewise rejects an in-flight action even when its
        // session is still attached at close time.
        let again = json(&cdp.cdp_request(
            connection,
            r#"{"id":5,"method":"Target.attachToTarget","params":{"targetId":"page-1","flatten":true}}"#,
        ).unwrap());
        let session = again["result"]["sessionId"].as_str().unwrap().to_string();
        cdp.poll_cdp_events(connection, 8).unwrap();
        let evaluate = format!(r#"{{"id":6,"sessionId":"{session}","method":"Runtime.evaluate","params":{{"expression":"1"}}}}"#);
        let queued = json(&cdp.cdp_request(connection, &evaluate).unwrap());
        let action_id = queued["result"]["obscuraAction"]["actionId"].as_u64().unwrap() as u32;
        let close = json(&cdp.cdp_request(
            connection,
            r#"{"id":7,"method":"Target.closeTarget","params":{"targetId":"page-1"}}"#,
        ).unwrap());
        assert_eq!(close["result"]["success"], true);
        assert!(cdp.complete_action(action_id, r#"{}"#).is_err());
        let events = json(&cdp.poll_cdp_events(connection, 8).unwrap());
        assert_eq!(events.as_array().unwrap().len(), 1);
        assert_eq!(events[0]["method"], "Target.detachedFromTarget");
        assert_eq!(events[0]["params"]["sessionId"], session);
    }

    #[cfg(feature = "render")]
    #[test]
    fn portable_render_commands_complete_inside_wasm() {
        let mut cdp = PortableCdp::new("<html><body><h1>WASM</h1></body></html>").unwrap();
        let connection = cdp.open_connection().unwrap();
        let attached = json(&cdp
            .cdp_request(
                connection,
                r#"{"id":1,"method":"Target.attachToTarget","params":{"targetId":"page-1"}}"#,
            )
            .unwrap());
        let session = attached["result"]["sessionId"].as_str().unwrap().to_string();
        cdp.poll_cdp_events(connection, 8).unwrap();

        let screenshot = json(&cdp
            .cdp_request(
                connection,
                &format!(r#"{{"id":2,"sessionId":"{session}","method":"Page.captureScreenshot","params":{{"format":"png"}}}}"#),
            )
            .unwrap());
        assert!(screenshot["error"].is_null(), "{screenshot}");
        let png = BASE64.decode(screenshot["result"]["data"].as_str().unwrap()).unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));

        let pdf = json(&cdp
            .cdp_request(
                connection,
                &format!(r#"{{"id":3,"sessionId":"{session}","method":"Page.printToPDF","params":{{"transferMode":"ReturnAsBase64"}}}}"#),
            )
            .unwrap());
        assert!(pdf["error"].is_null(), "{pdf}");
        let bytes = BASE64.decode(pdf["result"]["data"].as_str().unwrap()).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(bytes.ends_with(b"%%EOF\n"));
    }
}

/// Make the existing portable dispatcher usable through the shared CDP
/// engine contract. The wire-facing methods above remain stable for current
/// hosts; this adapter is the cutover seam for the future shared domain
/// dispatcher and keeps browser identity/action ownership in `obscura-cdp`.
impl CdpEngine for PortableCdp {
    fn create_context(&mut self, options: ContextOptions) -> Result<ContextId, CdpFailure> {
        let id = self.shared_state.create_context(options)?;
        let wire_id = format!("context-{}", id.get());
        self.shared_contexts.insert(wire_id.clone(), id);
        self.contexts.insert(wire_id);
        Ok(id)
    }

    fn dispose_context(&mut self, id: &ContextId) -> Result<(), CdpFailure> {
        let wire_id = self
            .shared_contexts
            .iter()
            .find_map(|(wire_id, shared_id)| (shared_id == id).then_some(wire_id.clone()))
            .ok_or(CdpFailure::UnknownContext(*id))?;
        if wire_id == "default" {
            return Err(CdpFailure::invalid_argument("default context cannot be disposed"));
        }
        let doomed: Vec<String> = self
            .targets
            .values()
            .filter(|target| target.context_id == wire_id)
            .map(|target| target.id.clone())
            .collect();
        self.shared_state.dispose_context(id)?;
        self.contexts.remove(&wire_id);
        self.shared_contexts.remove(&wire_id);
        for target_id in doomed {
            self.destroy_target(&target_id);
        }
        Ok(())
    }

    fn create_page(&mut self, context: &ContextId, url: &str) -> Result<PageId, CdpFailure> {
        let wire_context = self
            .shared_contexts
            .iter()
            .find_map(|(wire_id, shared_id)| (shared_id == context).then_some(wire_id.clone()))
            .ok_or(CdpFailure::UnknownContext(*context))?;
        let wire_page = self.create_target(&wire_context, url, "");
        self.shared_pages
            .get(&wire_page)
            .copied()
            .ok_or_else(|| CdpFailure::UnknownPage(PageId::new(0)))
    }

    fn close_page(&mut self, page: &PageId) -> Result<(), CdpFailure> {
        let wire_page = self
            .shared_pages
            .iter()
            .find_map(|(wire_id, shared_id)| (shared_id == page).then_some(wire_id.clone()))
            .ok_or(CdpFailure::UnknownPage(*page))?;
        if !self.targets.contains_key(&wire_page) {
            return Err(CdpFailure::UnknownPage(*page));
        }
        self.destroy_target(&wire_page);
        Ok(())
    }

    fn page_snapshot(&self, page: &PageId) -> Result<obscura_cdp::engine::PageSnapshot, CdpFailure> {
        let wire_page = self
            .shared_pages
            .iter()
            .find_map(|(wire_id, shared_id)| (shared_id == page).then_some(wire_id))
            .ok_or(CdpFailure::UnknownPage(*page))?;
        let target = self.targets.get(wire_page).ok_or(CdpFailure::UnknownPage(*page))?;
        let context_id = self
            .shared_contexts
            .get(&target.context_id)
            .copied()
            .ok_or_else(|| CdpFailure::UnknownContext(ContextId::new(0)))?;
        Ok(obscura_cdp::engine::PageSnapshot {
            page_id: *page,
            context_id,
            url: target.url.clone(),
            title: target.title.clone(),
            frame_id: target.frame_id.clone(),
            loader_id: target.loader_id.clone(),
            document_generation: u64::from(target.revision),
        })
    }

    fn start_action(&mut self, action: EngineAction) -> Result<EngineActionId, CdpFailure> {
        self.shared_state.start_action(action)
    }

    fn complete_action(&mut self, id: EngineActionId, result: EngineActionResult) -> Result<(), CdpFailure> {
        self.shared_state.complete_action(id, result)
    }
}

#[cfg(test)]
mod shared_engine_tests {
    use super::*;

    #[test]
    fn shared_engine_adapter_owns_page_lifecycle() {
        let mut cdp = PortableCdp::new("").unwrap();
        let context = ContextId::new(1);
        let page = CdpEngine::create_page(&mut cdp, &context, "https://example.test/").unwrap();
        let snapshot = CdpEngine::page_snapshot(&cdp, &page).unwrap();
        assert_eq!(snapshot.page_id, page);
        assert_eq!(snapshot.context_id, context);
        assert_eq!(snapshot.url, "https://example.test/");
        CdpEngine::close_page(&mut cdp, &page).unwrap();
        assert_eq!(CdpEngine::page_snapshot(&cdp, &page), Err(CdpFailure::UnknownPage(page)));
    }
}
