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

struct Target {
    id: String,
    context_id: String,
    frame_id: String,
    loader_id: String,
    url: String,
    title: String,
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
        let Some(action) = self.actions.remove(&action_id) else {
            return Err(js_error("stale or unknown CDP action"));
        };
        let result: Value = serde_json::from_str(result_json)
            .map_err(|error| js_error(&format!("invalid CDP action result: {error}")))?;
        if action.kind == "navigate" {
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
            }
        }
        let response = cdp_result_response(&action.request_id, result, action.session_id.as_deref());
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
                self.detach_session(connection_id, session_id);
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
            "DOM.getDocument" => {
                let Some(target) = self.targets.get(&target_id) else {
                    return cdp_error_response(&request.id, -32000, "target is not known", session);
                };
                let document_handle = target.core.document_handle();
                cdp_result_response(&request.id, json!({"root": {
                    "nodeId": document_handle,
                    "backendNodeId": document_handle,
                    "nodeType": 9,
                    "nodeName": "#document",
                    "localName": "",
                    "nodeValue": "",
                    "childNodeCount": 0,
                    "children": [],
                    "documentURL": target.url,
                    "baseURL": target.url,
                }}), session)
            }
            "Page.navigate" => self.queue_action(connection_id, request.clone(), target_id, "navigate", json!({
                "url": request.params.get("url").and_then(Value::as_str).unwrap_or("about:blank"),
                "method": request.params.get("referrer").and_then(Value::as_str).unwrap_or("GET"),
            })),
            "Runtime.evaluate" => self.queue_action(connection_id, request.clone(), target_id, "evaluate", json!({
                "expression": request.params.get("expression").and_then(Value::as_str).unwrap_or(""),
                "returnByValue": request.params.get("returnByValue").and_then(Value::as_bool).unwrap_or(false),
            })),
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
            core,
            sessions: BTreeSet::new(),
        });
        target_id
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

    fn detach_session(&mut self, connection_id: u32, session_id: &str) {
        let target_id = self.connections.get_mut(&connection_id).and_then(|connection| connection.sessions.remove(session_id));
        if let Some(target_id) = target_id {
            if let Some(target) = self.targets.get_mut(&target_id) {
                target.sessions.remove(session_id);
            }
        }
    }

    fn destroy_target(&mut self, target_id: &str) {
        let Some(target) = self.targets.remove(target_id) else { return; };
        for connection in self.connections.values_mut() {
            let doomed: Vec<String> = connection.sessions.iter().filter(|(_, id)| *id == &target.id).map(|(session, _)| session.clone()).collect();
            for session_id in doomed {
                connection.sessions.remove(&session_id);
                queue_event(connection, "Target.detachedFromTarget", json!({"sessionId": session_id, "targetId": target.id}), None);
            }
            if connection.discover_targets {
                queue_event(connection, "Target.targetDestroyed", json!({"targetId": target.id}), None);
            }
        }
        self.actions.retain(|_, action| action.target_id != target.id);
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
}
