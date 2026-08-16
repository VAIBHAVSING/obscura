//! Transport-independent CDP state for the portable browser.
//!
//! This module deliberately does not depend on Tokio, sockets, threads, or a
//! WebSocket implementation. A host owns the transport and calls the small
//! JSON ABI below. Commands which need host services (JavaScript evaluation,
//! navigation I/O, screenshot, and PDF encoding) are represented as bounded,
//! opaque actions. The host completes an action after doing the platform work;
//! response and event ordering remain owned here.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use serde::Deserialize;
use serde_json::{json, Map, Value};
use url::Url;
use wasm_bindgen::prelude::*;

use crate::ObscuraCore;

pub const CDP_ABI_VERSION: u32 = 1;
const MAX_MESSAGE_BYTES: usize = 1 * 1024 * 1024;
const MAX_METHOD_BYTES: usize = 256;
const MAX_SESSION_BYTES: usize = 256;
const MAX_EVENT_QUEUE: usize = 512;
const MAX_EVENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ACTION_RESULT_BYTES: usize = 16 * 1024 * 1024;

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
    targets: BTreeMap<String, Target>,
    connections: BTreeMap<u32, Connection>,
    actions: BTreeMap<u32, Action>,
    contexts: BTreeSet<String>,
    next_connection_id: u32,
    next_target_id: u32,
    next_session_id: u32,
    next_action_id: u32,
    next_loader_id: u32,
}

#[wasm_bindgen]
impl PortableCdp {
    #[wasm_bindgen(constructor)]
    pub fn new(html: &str) -> Result<Self, JsValue> {
        let core = ObscuraCore::new(html)?;
        let document_handle = core.document_handle();
        let revision = core.page_revision();
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
                core,
                sessions: BTreeSet::new(),
            },
        );
        let mut contexts = BTreeSet::new();
        contexts.insert("default".to_string());
        Ok(Self {
            targets,
            connections: BTreeMap::new(),
            actions: BTreeMap::new(),
            contexts,
            next_connection_id: 0,
            next_target_id: 1,
            next_session_id: 0,
            next_action_id: 0,
            next_loader_id: 0,
        })
    }

    /// Register one host connection and return its opaque ID.
    #[wasm_bindgen(js_name = openConnection)]
    pub fn open_connection(&mut self) -> Result<u32, JsValue> {
        let id = self
            .next_connection_id
            .checked_add(1)
            .ok_or_else(|| js_range_error("CDP connection ID space is exhausted"))?;
        self.next_connection_id = id;
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
                }
            }
        }
        self.actions
            .retain(|_, action| action.connection_id != connection_id);
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
            return Err(js_error("CDP action target session is no longer live"));
        }
        self.actions.remove(&action_id);
        let result: Value = serde_json::from_str(result_json)
            .map_err(|error| js_error(&format!("invalid CDP action result: {error}")))?;
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
            "pendingActions": self.actions.len(),
            "eventQueueLimit": MAX_EVENT_QUEUE,
            "actionResultBytes": MAX_ACTION_RESULT_BYTES,
        })
        .to_string()
    }
}

impl PortableCdp {
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
                let id = format!("context-{}", self.contexts.len());
                self.contexts.insert(id.clone());
                cdp_result_response(&request.id, json!({"browserContextId": id}), session)
            }
            "Target.disposeBrowserContext" => {
                let Some(id) = request.params.get("browserContextId").and_then(Value::as_str) else {
                    return cdp_error_response(&request.id, -32602, "browserContextId is required", session);
                };
                if id == "default" || !self.contexts.remove(id) {
                    return cdp_error_response(&request.id, -32000, "browser context cannot be disposed", session);
                }
                let doomed: Vec<String> = self
                    .targets
                    .values()
                    .filter(|target| target.context_id == id)
                    .map(|target| target.id.clone())
                    .collect();
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

    fn dispatch_page(&mut self, connection_id: u32, target_id: String, session_id: Option<String>, request: Request) -> Value {
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
            "Page.enable" | "Network.enable" | "DOM.enable" | "Runtime.disable" | "Page.disable" | "Network.disable" | "DOM.disable" => {
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
            "Network.clearBrowserCache" => cdp_result_response(&request.id, json!({}), session),
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
            "Page.setDocumentContent" => self.queue_action(connection_id, request.clone(), target_id, "setDocumentContent", json!({
                "html": request.params.get("html").and_then(Value::as_str).unwrap_or(""),
            })),
            "Page.navigate" => self.queue_action(connection_id, request.clone(), target_id, "navigate", json!({
                "url": request.params.get("url").and_then(Value::as_str).unwrap_or("about:blank"),
                "method": request.params.get("referrer").and_then(Value::as_str).unwrap_or("GET"),
            })),
            "Page.reload" => {
                let url = self.targets.get(&target_id).map(|target| target.url.clone()).unwrap_or_else(|| "about:blank".to_string());
                self.queue_action(connection_id, request.clone(), target_id, "reload", json!({"url": url}))
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
            "Page.captureScreenshot" => self.queue_action(connection_id, request.clone(), target_id, "screenshot", json!({
                "format": request.params.get("format").and_then(Value::as_str).unwrap_or("png"),
            })),
            "Page.printToPDF" => self.queue_action(connection_id, request.clone(), target_id, "pdf", json!({
                "landscape": request.params.get("landscape").and_then(Value::as_bool).unwrap_or(false),
            })),
            _ => cdp_error_response(&request.id, -32601, "method is not implemented", session),
        }
    }

    fn queue_action(&mut self, connection_id: u32, request: Request, target_id: String, kind: &str, payload: Value) -> Value {
        let action_id = match self.next_action_id.checked_add(1) {
            Some(value) => value,
            None => return cdp_error_response(&request.id, -32000, "CDP action ID space is exhausted", request.session_id.as_deref()),
        };
        self.next_action_id = action_id;
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

    fn create_target(&mut self, context_id: &str, url: &str, html: &str) -> String {
        self.next_target_id = self.next_target_id.saturating_add(1);
        let target_id = format!("page-{}", self.next_target_id);
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
        }
        self.actions.retain(|_, action| {
            action.connection_id != connection_id || action.session_id.as_deref() != Some(session_id)
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
        // A target close can arrive while its host operation is in flight.
        // Completing one of those actions must be rejected, not applied to a
        // subsequent page with a coincidentally similar identifier.
        self.actions.retain(|_, action| action.target_id != target.id);
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

const MAX_CDP_COOKIE_COUNT: usize = 4096;
const MAX_CDP_COOKIE_BYTES: usize = 64 * 1024;

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
}
