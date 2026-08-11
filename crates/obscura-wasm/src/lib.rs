use std::any::Any;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use obscura_dom::{
    parse_fragment, parse_fragment_with_context, parse_html, DomTree, NodeData, NodeId,
};
use wasm_bindgen::prelude::*;

const ABI_VERSION: u32 = 1;
const DOM_OP_ABI_VERSION: u32 = 1;
const DOM_BATCH_ABI_VERSION: u32 = 1;
const MAX_HTML_INPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SELECTOR_BYTES: usize = 64 * 1024;
const MAX_RETURNED_STRING_BYTES: usize = 4 * 1024 * 1024;
const MAX_DOM_COMMAND_BYTES: usize = 64;
const MAX_DOM_ARGUMENT_BYTES: usize = MAX_HTML_INPUT_BYTES;
const MAX_DOM_BATCH_BYTES: usize = 8 * 1024 * 1024;
const MAX_DOM_BATCH_OPS: usize = 1024;
const MAX_DOCUMENT_METADATA_BYTES: usize = 64 * 1024;

fn require_max_bytes(value: &str, maximum: usize, label: &str) -> Result<(), JsValue> {
    if value.len() > maximum {
        return Err(js_sys::RangeError::new(&format!(
            "{label} exceeds the {maximum}-byte ABI limit"
        ))
        .into());
    }
    Ok(())
}

fn bounded_return(value: String, label: &str) -> Result<String, JsValue> {
    require_max_bytes(&value, MAX_RETURNED_STRING_BYTES, label)?;
    Ok(value)
}

fn panic_message(operation: &str, payload: Box<dyn Any + Send>) -> String {
    let detail = if let Some(message) = payload.downcast_ref::<&str>() {
        *message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else {
        "unknown Rust panic"
    };
    format!("Obscura WASM {operation} panicked: {detail}")
}

fn boundary_value<T>(operation: &str, call: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(value) => value,
        Err(payload) => {
            wasm_bindgen::throw_val(js_sys::Error::new(&panic_message(operation, payload)).into())
        }
    }
}

fn boundary_result<T>(
    operation: &str,
    call: impl FnOnce() -> Result<T, String>,
) -> Result<T, JsValue> {
    boundary_result_with(operation, call, |error| js_sys::Error::new(&error).into())
}

fn boundary_selector_result<T>(
    operation: &str,
    call: impl FnOnce() -> Result<T, String>,
) -> Result<T, JsValue> {
    boundary_result_with(operation, call, |error| {
        js_sys::SyntaxError::new(&error).into()
    })
}

fn boundary_result_with<T>(
    operation: &str,
    call: impl FnOnce() -> Result<T, String>,
    error_value: impl FnOnce(String) -> JsValue,
) -> Result<T, JsValue> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error_value(error)),
        Err(payload) => Err(js_sys::Error::new(&panic_message(operation, payload)).into()),
    }
}

/// Portable part of an Obscura page.
///
/// JavaScript execution deliberately belongs to the host runtime. In Node,
/// that is the V8 isolate already owned by Node; embedding rusty_v8 in this
/// wasm module would create a nested VM and is not a supported wasm32 target.
#[wasm_bindgen]
pub struct ObscuraCore {
    dom: DomTree,
    /// Opaque handles are never recycled, even when `set_html` replaces the
    /// complete arena. This prevents a wrapper retained by host JavaScript
    /// from silently aliasing an unrelated node in the next document.
    handle_to_node: HashMap<u32, NodeId>,
    node_to_handle: HashMap<NodeId, u32>,
    next_handle: u32,
    document_handle: u32,
    page_revision: u32,
    document_url: String,
    document_referrer: String,
    document_encoding: String,
}

#[wasm_bindgen]
impl ObscuraCore {
    #[wasm_bindgen(constructor)]
    pub fn new(html: &str) -> Result<Self, JsValue> {
        require_max_bytes(html, MAX_HTML_INPUT_BYTES, "HTML input")?;
        let dom = boundary_value("constructor", || parse_html(html));
        let document = dom.document();
        let mut handle_to_node = HashMap::new();
        let mut node_to_handle = HashMap::new();
        handle_to_node.insert(1, document);
        node_to_handle.insert(document, 1);
        Ok(Self {
            dom,
            handle_to_node,
            node_to_handle,
            next_handle: 2,
            document_handle: 1,
            page_revision: 0,
            document_url: "about:blank".to_string(),
            document_referrer: String::new(),
            document_encoding: "UTF-8".to_string(),
        })
    }

    /// Replace the document using Obscura's existing html5ever-backed parser.
    pub fn set_html(&mut self, html: &str) -> Result<(), JsValue> {
        require_max_bytes(html, MAX_HTML_INPUT_BYTES, "HTML input")?;
        boundary_result("set_html", || {
            let next_revision = self
                .page_revision
                .checked_add(1)
                .ok_or_else(|| "page revision space is exhausted".to_string())?;
            let document_handle = self.reserve_handle()?;
            let dom = parse_html(html);
            let document = dom.document();
            self.dom = dom;
            self.handle_to_node.clear();
            self.node_to_handle.clear();
            self.handle_to_node.insert(document_handle, document);
            self.node_to_handle.insert(document, document_handle);
            self.document_handle = document_handle;
            self.page_revision = next_revision;
            Ok(())
        })
    }

    /// Opaque identity for the current document node.
    #[wasm_bindgen(js_name = documentHandle)]
    pub fn document_handle(&self) -> u32 {
        self.document_handle
    }

    /// Monotonic invalidation revision for host-side DOM wrappers and caches.
    #[wasm_bindgen(js_name = pageRevision)]
    pub fn page_revision(&self) -> u32 {
        self.page_revision
    }

    /// Supply navigation metadata used by the op_dom-compatible document
    /// facade. Metadata changes do not invalidate DOM node wrappers.
    #[wasm_bindgen(js_name = setDocumentMetadata)]
    pub fn set_document_metadata(
        &mut self,
        url: &str,
        referrer: &str,
        encoding: &str,
    ) -> Result<(), JsValue> {
        require_max_bytes(url, MAX_DOCUMENT_METADATA_BYTES, "document URL")?;
        require_max_bytes(referrer, MAX_DOCUMENT_METADATA_BYTES, "document referrer")?;
        require_max_bytes(encoding, MAX_DOCUMENT_METADATA_BYTES, "document encoding")?;
        self.document_url = url.to_string();
        self.document_referrer = referrer.to_string();
        self.document_encoding = encoding.to_string();
        Ok(())
    }

    /// Execute one command using the same three-string wire contract as the
    /// native `op_dom`. Node ids in that protocol are opaque handles here.
    #[wasm_bindgen(js_name = domOp)]
    pub fn dom_op(&mut self, cmd: &str, arg1: &str, arg2: &str) -> Result<String, JsValue> {
        require_max_bytes(cmd, MAX_DOM_COMMAND_BYTES, "DOM command")?;
        require_max_bytes(arg1, MAX_DOM_ARGUMENT_BYTES, "DOM argument")?;
        require_max_bytes(arg2, MAX_DOM_ARGUMENT_BYTES, "DOM argument")?;
        boundary_result("dom_op", || self.dom_op_panic_safe(cmd, arg1, arg2))
            .and_then(|value| bounded_return(value, "DOM operation result"))
    }

    /// Execute an ordered, non-transactional batch of `op_dom` commands.
    ///
    /// Input is `[[cmd, arg1, arg2], ...]`; output is a JSON array containing
    /// each command's ordinary string result in the same order.
    #[wasm_bindgen(js_name = domBatch)]
    pub fn dom_batch(&mut self, request: &str) -> Result<String, JsValue> {
        require_max_bytes(request, MAX_DOM_BATCH_BYTES, "DOM batch request")?;
        boundary_result("dom_batch", || self.dom_batch_inner(request))
            .and_then(|value| bounded_return(value, "DOM batch response"))
    }

    /// Serialize the complete document.
    pub fn html(&self) -> Result<String, JsValue> {
        bounded_return(
            boundary_value("html", || self.dom.outer_html(self.dom.document())),
            "serialized document",
        )
    }

    /// Serialize the document element without the document doctype.
    ///
    /// This is kept separate from `html()` because browsers expose these as
    /// different values: `document.documentElement.outerHTML` is the `<html>`
    /// element, while serializing the document may also include its doctype.
    pub fn document_element_html(&self) -> Result<String, JsValue> {
        let html = boundary_result("document_element_html", || {
            let node = self
                .dom
                .query_selector("html")?
                .ok_or_else(|| "document has no html element".to_string())?;
            Ok(self.dom.outer_html(node))
        })?;
        bounded_return(html, "serialized document element")
    }

    /// Return the first matching element's serialized HTML, or `undefined`.
    pub fn query_html(&self, selector: &str) -> Result<Option<String>, JsValue> {
        require_max_bytes(selector, MAX_SELECTOR_BYTES, "selector")?;
        let html = boundary_selector_result("query_html", || {
            self.dom
                .query_selector(selector)
                .map(|node| node.map(|node| self.dom.outer_html(node)))
        })?;
        html.map(|html| bounded_return(html, "query outerHTML"))
            .transpose()
    }

    /// Return the first matching element's textContent, or `undefined`.
    pub fn query_text(&self, selector: &str) -> Result<Option<String>, JsValue> {
        require_max_bytes(selector, MAX_SELECTOR_BYTES, "selector")?;
        let text = boundary_selector_result("query_text", || {
            self.dom
                .query_selector(selector)
                .map(|node| node.map(|node| self.dom.text_content(node)))
        })?;
        text.map(|text| bounded_return(text, "query textContent"))
            .transpose()
    }

    /// Return `[outerHTML, textContent]` for the first match, or `undefined`.
    pub fn query_snapshot(&self, selector: &str) -> Result<JsValue, JsValue> {
        require_max_bytes(selector, MAX_SELECTOR_BYTES, "selector")?;
        let snapshot = boundary_selector_result("query_snapshot", || {
            self.dom.query_selector(selector).map(|node| {
                node.map(|node| (self.dom.outer_html(node), self.dom.text_content(node)))
            })
        })?;
        let Some((outer_html, text_content)) = snapshot else {
            return Ok(JsValue::UNDEFINED);
        };
        let outer_html = bounded_return(outer_html, "query outerHTML")?;
        let text_content = bounded_return(text_content, "query textContent")?;
        let result = js_sys::Array::new_with_length(2);
        result.set(0, JsValue::from_str(&outer_html));
        result.set(1, JsValue::from_str(&text_content));
        Ok(result.into())
    }

    pub fn query_count(&self, selector: &str) -> Result<u32, JsValue> {
        require_max_bytes(selector, MAX_SELECTOR_BYTES, "selector")?;
        let count = boundary_selector_result("query_count", || {
            self.dom
                .query_selector_all(selector)
                .map(|nodes| nodes.len())
        })?;
        u32::try_from(count).map_err(|_| js_sys::Error::new("selector result exceeds u32").into())
    }
}

impl ObscuraCore {
    fn reserve_handle(&mut self) -> Result<u32, String> {
        let handle = self.next_handle;
        self.next_handle = handle
            .checked_add(1)
            .ok_or_else(|| "node handle space is exhausted".to_string())?;
        Ok(handle)
    }

    fn expose_node(&mut self, node: NodeId) -> Result<u32, String> {
        if self.dom.get_node(node).is_none() {
            return Err(format!("cannot expose missing DOM node {}", node.raw()));
        }
        if let Some(handle) = self.node_to_handle.get(&node) {
            return Ok(*handle);
        }
        let handle = self.reserve_handle()?;
        self.handle_to_node.insert(handle, node);
        self.node_to_handle.insert(node, handle);
        Ok(handle)
    }

    fn resolve_handle(&self, value: &str) -> Result<NodeId, String> {
        let handle = value
            .parse::<u32>()
            .map_err(|_| format!("invalid node handle {value:?}"))?;
        let node = self
            .handle_to_node
            .get(&handle)
            .copied()
            .ok_or_else(|| format!("stale or unknown node handle {handle}"))?;
        if self.dom.get_node(node).is_none() {
            return Err(format!("stale or unknown node handle {handle}"));
        }
        Ok(node)
    }

    fn expose_optional_node(&mut self, node: Option<NodeId>) -> Result<String, String> {
        match node {
            Some(node) => Ok(self.expose_node(node)?.to_string()),
            None => Ok("-1".to_string()),
        }
    }

    fn expose_nodes_json(&mut self, nodes: Vec<NodeId>) -> Result<String, String> {
        let handles = nodes
            .into_iter()
            .map(|node| self.expose_node(node))
            .collect::<Result<Vec<_>, _>>()?;
        serde_json::to_string(&handles).map_err(|error| error.to_string())
    }

    fn validate_dom_args(cmd: &str, arg1: &str, arg2: &str) -> Result<(), String> {
        if cmd.len() > MAX_DOM_COMMAND_BYTES {
            return Err(format!(
                "DOM command exceeds the {MAX_DOM_COMMAND_BYTES}-byte ABI limit"
            ));
        }
        if arg1.len() > MAX_DOM_ARGUMENT_BYTES || arg2.len() > MAX_DOM_ARGUMENT_BYTES {
            return Err(format!(
                "DOM argument exceeds the {MAX_DOM_ARGUMENT_BYTES}-byte ABI limit"
            ));
        }
        let selector = match cmd {
            "query_selector" | "query_selector_all" => Some(arg1),
            "query_selector_scoped" | "query_selector_all_scoped" | "matches_selector" => {
                Some(arg2)
            }
            _ => None,
        };
        if selector.is_some_and(|selector| selector.len() > MAX_SELECTOR_BYTES) {
            return Err(format!(
                "selector exceeds the {MAX_SELECTOR_BYTES}-byte ABI limit"
            ));
        }
        Ok(())
    }

    fn dom_batch_inner(&mut self, request: &str) -> Result<String, String> {
        let value: serde_json::Value = serde_json::from_str(request)
            .map_err(|error| format!("invalid DOM batch JSON: {error}"))?;
        let entries = value
            .as_array()
            .ok_or_else(|| "DOM batch must be a JSON array".to_string())?;
        if entries.len() > MAX_DOM_BATCH_OPS {
            return Err(format!(
                "DOM batch exceeds the {MAX_DOM_BATCH_OPS}-operation ABI limit"
            ));
        }

        // Validate and own every string before executing the first command, so
        // malformed envelopes never cause a partial batch.
        let mut commands = Vec::with_capacity(entries.len());
        for (index, entry) in entries.iter().enumerate() {
            let tuple = entry
                .as_array()
                .filter(|tuple| tuple.len() == 3)
                .ok_or_else(|| format!("DOM batch entry {index} must contain exactly 3 strings"))?;
            let cmd = tuple[0]
                .as_str()
                .ok_or_else(|| format!("DOM batch entry {index} command must be a string"))?;
            let arg1 = tuple[1]
                .as_str()
                .ok_or_else(|| format!("DOM batch entry {index} arg1 must be a string"))?;
            let arg2 = tuple[2]
                .as_str()
                .ok_or_else(|| format!("DOM batch entry {index} arg2 must be a string"))?;
            Self::validate_dom_args(cmd, arg1, arg2)?;
            commands.push((cmd.to_string(), arg1.to_string(), arg2.to_string()));
        }

        let mut results = Vec::with_capacity(commands.len());
        for (cmd, arg1, arg2) in commands {
            results.push(self.dom_op_panic_safe(&cmd, &arg1, &arg2)?);
        }
        serde_json::to_string(&results).map_err(|error| error.to_string())
    }

    /// Match native `op_dom`'s anti-panic contract: a defect in one DOM
    /// command degrades to that command's ordinary `null` result and never
    /// unwinds through the host VM boundary. A batch therefore retains its
    /// ordered, per-operation semantics even when one command panics.
    fn dom_op_panic_safe(
        &mut self,
        cmd: &str,
        arg1: &str,
        arg2: &str,
    ) -> Result<String, String> {
        match catch_unwind(AssertUnwindSafe(|| self.dom_op_inner(cmd, arg1, arg2))) {
            Ok(result) => result,
            Err(_) => Ok("null".to_string()),
        }
    }

    fn dom_op_inner(&mut self, cmd: &str, arg1: &str, arg2: &str) -> Result<String, String> {
        Self::validate_dom_args(cmd, arg1, arg2)?;
        let next_revision = if is_dom_mutation_command(cmd) {
            Some(
                self.page_revision
                    .checked_add(1)
                    .ok_or_else(|| "page revision space is exhausted".to_string())?,
            )
        } else {
            None
        };
        let result = self.dispatch_dom_op(cmd, arg1, arg2)?;
        if let Some(next_revision) = next_revision {
            // Native op_dom reports `false`/`-1` for rejected tree mutations.
            // Do not invalidate host caches when no mutation took place.
            if result != "false" && result != "-1" {
                self.page_revision = next_revision;
            }
        }
        Ok(result)
    }

    fn dispatch_dom_op(&mut self, cmd: &str, arg1: &str, arg2: &str) -> Result<String, String> {
        match cmd {
            "document_node_id" => Ok(self.document_handle.to_string()),
            "document_title" => {
                let title = self
                    .dom
                    .query_selector("title")
                    .ok()
                    .flatten()
                    .map(|title| {
                        self.dom
                            .text_content(title)
                            .split(|ch| matches!(ch, '\t' | '\n' | '\u{000C}' | '\r' | ' '))
                            .filter(|part| !part.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                serde_json::to_string(&title).map_err(|error| error.to_string())
            }
            "document_url" => {
                serde_json::to_string(&self.document_url).map_err(|error| error.to_string())
            }
            "document_referrer" => {
                serde_json::to_string(&self.document_referrer).map_err(|error| error.to_string())
            }
            "document_encoding" => {
                serde_json::to_string(&self.document_encoding).map_err(|error| error.to_string())
            }
            "document_element" => {
                let node = self
                    .dom
                    .children(self.dom.document())
                    .into_iter()
                    .find(|node| {
                        self.dom
                            .get_node(*node)
                            .and_then(|node| node.as_element().cloned())
                            .is_some_and(|name| name.local.as_ref() == "html")
                    });
                self.expose_optional_node(node)
            }
            "document_doctype" => {
                let doctype = self
                    .dom
                    .children(self.dom.document())
                    .into_iter()
                    .find_map(|node| {
                        self.dom.get_node(node).and_then(|entry| match entry.data {
                            NodeData::Doctype {
                                name,
                                public_id,
                                system_id,
                            } => Some((node, name, public_id, system_id)),
                            _ => None,
                        })
                    });
                match doctype {
                    Some((node, name, public_id, system_id)) => {
                        let handle = self.expose_node(node)?;
                        Ok(serde_json::json!({
                            "name": name,
                            "publicId": public_id,
                            "systemId": system_id,
                            "nodeId": handle,
                        })
                        .to_string())
                    }
                    None => Ok("null".to_string()),
                }
            }
            "get_element_by_id" => {
                let document = self.dom.document();
                let indexed = self.dom.get_element_by_id(arg1);
                let live = indexed.filter(|node| self.dom.ancestors(*node).contains(&document));
                let node = live.or_else(|| {
                    let selector = format!(
                        "[id=\"{}\"]",
                        arg1.replace('\\', "\\\\").replace('"', "\\\"")
                    );
                    self.dom.query_selector(&selector).ok().flatten()
                });
                self.expose_optional_node(node)
            }
            "query_selector" => {
                let node = self.dom.query_selector(arg1).ok().flatten();
                self.expose_optional_node(node)
            }
            "query_selector_all" => {
                let nodes = self.dom.query_selector_all(arg1).unwrap_or_default();
                self.expose_nodes_json(nodes)
            }
            "query_selector_scoped" => {
                let root = self.resolve_handle(arg1)?;
                let node = self.dom.query_selector_from(root, arg2).ok().flatten();
                self.expose_optional_node(node)
            }
            "query_selector_all_scoped" => {
                let root = self.resolve_handle(arg1)?;
                let nodes = self
                    .dom
                    .query_selector_all_from(root, arg2)
                    .unwrap_or_default();
                self.expose_nodes_json(nodes)
            }
            "matches_selector" => {
                let node = self.resolve_handle(arg1)?;
                Ok(self
                    .dom
                    .matches_selector(node, arg2)
                    .unwrap_or(false)
                    .to_string())
            }
            "node_type" => {
                let node = self.resolve_handle(arg1)?;
                Ok(self
                    .dom
                    .with_node(node, |node| match &node.data {
                        NodeData::Document => "9",
                        NodeData::Element { .. } => "1",
                        NodeData::Text { .. } => "3",
                        NodeData::Comment { .. } => "8",
                        NodeData::Doctype { .. } => "10",
                        NodeData::ProcessingInstruction { .. } => "7",
                    })
                    .unwrap_or("0")
                    .to_string())
            }
            "node_name" => {
                let node = self.resolve_handle(arg1)?;
                let name = self
                    .dom
                    .with_node(node, |node| match &node.data {
                        NodeData::Document => "#document".to_string(),
                        NodeData::Element { name, .. } => name.local.as_ref().to_ascii_uppercase(),
                        NodeData::Text { .. } => "#text".to_string(),
                        NodeData::Comment { .. } => "#comment".to_string(),
                        NodeData::Doctype { name, .. } => name.clone(),
                        NodeData::ProcessingInstruction { target, .. } => target.clone(),
                    })
                    .unwrap_or_default();
                serde_json::to_string(&name).map_err(|error| error.to_string())
            }
            "text_content" => {
                let node = self.resolve_handle(arg1)?;
                serde_json::to_string(&self.dom.text_content(node))
                    .map_err(|error| error.to_string())
            }
            "parent_node" | "first_child" | "last_child" | "next_sibling" | "prev_sibling" => {
                let node = self.resolve_handle(arg1)?;
                let related = self
                    .dom
                    .with_node(node, |node| match cmd {
                        "parent_node" => node.parent,
                        "first_child" => node.first_child,
                        "last_child" => node.last_child,
                        "next_sibling" => node.next_sibling,
                        "prev_sibling" => node.prev_sibling,
                        _ => None,
                    })
                    .flatten();
                self.expose_optional_node(related)
            }
            "next_in_subtree" | "prev_in_subtree" | "next_after_subtree" => {
                let root = self.resolve_handle(arg1)?;
                let current = self.resolve_handle(arg2)?;
                let related = match cmd {
                    "next_in_subtree" => self.dom.next_in_subtree(root, current),
                    "prev_in_subtree" => self.dom.prev_in_subtree(root, current),
                    "next_after_subtree" => self.dom.next_after_subtree(root, current),
                    _ => None,
                };
                self.expose_optional_node(related)
            }
            "child_nodes" => {
                let node = self.resolve_handle(arg1)?;
                self.expose_nodes_json(self.dom.children(node))
            }
            "tag_name" => {
                let node = self.resolve_handle(arg1)?;
                let name = self
                    .dom
                    .with_node(node, |node| {
                        node.as_element().map(|name| {
                            if name.ns.as_ref() == "http://www.w3.org/1999/xhtml" {
                                name.local.as_ref().to_ascii_uppercase()
                            } else {
                                match &name.prefix {
                                    Some(prefix) => format!("{}:{}", prefix, name.local),
                                    None => name.local.to_string(),
                                }
                            }
                        })
                    })
                    .flatten()
                    .unwrap_or_default();
                serde_json::to_string(&name).map_err(|error| error.to_string())
            }
            "local_name" => {
                let node = self.resolve_handle(arg1)?;
                let name = self
                    .dom
                    .with_node(node, |node| {
                        node.as_element().map(|name| name.local.to_string())
                    })
                    .flatten()
                    .unwrap_or_default();
                serde_json::to_string(&name).map_err(|error| error.to_string())
            }
            "namespace_uri" => {
                let node = self.resolve_handle(arg1)?;
                let namespace = self
                    .dom
                    .with_node(node, |node| {
                        node.as_element().map(|name| name.ns.as_ref().to_string())
                    })
                    .flatten()
                    .unwrap_or_default();
                serde_json::to_string(&namespace).map_err(|error| error.to_string())
            }
            "get_attribute" => {
                let node = self.resolve_handle(arg1)?;
                let value = self
                    .dom
                    .with_node(node, |node| node.get_attribute(arg2).map(str::to_string))
                    .flatten();
                serde_json::to_string(&value).map_err(|error| error.to_string())
            }
            "attribute_names" => {
                let node = self.resolve_handle(arg1)?;
                let names: Vec<String> = self
                    .dom
                    .with_node(node, |node| {
                        node.attrs()
                            .map(|attrs| attrs.iter().map(|attr| attr.qualified_name()).collect())
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                serde_json::to_string(&names).map_err(|error| error.to_string())
            }
            "set_attribute" => {
                let node = self.resolve_handle(arg1)?;
                if let Some((name, value)) = arg2.split_once('\0') {
                    if name == "id" {
                        let old_id = self
                            .dom
                            .with_node(node, |node| node.get_attribute("id").map(str::to_string))
                            .flatten();
                        self.dom.with_node_mut(node, |node| {
                            node.set_attribute(name, value.to_string())
                        });
                        self.dom
                            .update_id_index(node, old_id.as_deref(), Some(value));
                    } else {
                        self.dom.with_node_mut(node, |node| {
                            node.set_attribute(name, value.to_string())
                        });
                    }
                }
                Ok("true".to_string())
            }
            "inner_html" => {
                let node = self.resolve_handle(arg1)?;
                serde_json::to_string(&self.dom.inner_html(node)).map_err(|error| error.to_string())
            }
            "outer_html" => {
                let node = self.resolve_handle(arg1)?;
                serde_json::to_string(&self.dom.outer_html(node)).map_err(|error| error.to_string())
            }
            "append_child" => {
                let parent = self.resolve_handle(arg1)?;
                let child = self.resolve_handle(arg2)?;
                self.dom.append_child(parent, child);
                Ok(
                    (self.dom.get_node(child).and_then(|node| node.parent) == Some(parent))
                        .to_string(),
                )
            }
            "remove_child" => {
                let child = self.resolve_handle(arg1)?;
                let had_parent = self
                    .dom
                    .get_node(child)
                    .is_some_and(|node| node.parent.is_some());
                self.dom.remove_child(child);
                Ok((had_parent
                    && self
                        .dom
                        .get_node(child)
                        .is_some_and(|node| node.parent.is_none()))
                .to_string())
            }
            "insert_before" => {
                let new_node = self.resolve_handle(arg1)?;
                let reference = self.resolve_handle(arg2)?;
                let expected_parent = self.dom.get_node(reference).and_then(|node| node.parent);
                self.dom.insert_before(reference, new_node);
                Ok((expected_parent.is_some()
                    && self.dom.get_node(new_node).and_then(|node| node.parent) == expected_parent)
                    .to_string())
            }
            "remove_attribute" => {
                let node = self.resolve_handle(arg1)?;
                self.dom.with_node_mut(node, |node| {
                    if let NodeData::Element { attrs, .. } = &mut node.data {
                        attrs.retain(|attr| !attr.qualified_name_eq(arg2));
                    }
                });
                Ok("true".to_string())
            }
            "get_attribute_ns" => {
                let node = self.resolve_handle(arg1)?;
                let (namespace, local) = arg2.split_once('\0').unwrap_or(("", arg2));
                let value = self
                    .dom
                    .with_node(node, |node| {
                        node.get_attribute_ns(namespace, local).map(str::to_string)
                    })
                    .flatten();
                serde_json::to_string(&value).map_err(|error| error.to_string())
            }
            "set_attribute_ns" => {
                let node = self.resolve_handle(arg1)?;
                let mut parts = arg2.splitn(3, '\0');
                let namespace = parts.next().unwrap_or("");
                let qualified = parts.next().unwrap_or("");
                let value = parts.next().unwrap_or("");
                if !qualified.is_empty() {
                    let local = qualified
                        .split_once(':')
                        .map(|(_, local)| local)
                        .unwrap_or(qualified);
                    if namespace.is_empty() && local == "id" {
                        let old_id = self
                            .dom
                            .with_node(node, |node| node.get_attribute("id").map(str::to_string))
                            .flatten();
                        self.dom.with_node_mut(node, |node| {
                            node.set_attribute_ns(namespace, qualified, value.to_string())
                        });
                        self.dom
                            .update_id_index(node, old_id.as_deref(), Some(value));
                    } else {
                        self.dom.with_node_mut(node, |node| {
                            node.set_attribute_ns(namespace, qualified, value.to_string())
                        });
                    }
                }
                Ok("true".to_string())
            }
            "remove_attribute_ns" => {
                let node = self.resolve_handle(arg1)?;
                let (namespace, local) = arg2.split_once('\0').unwrap_or(("", arg2));
                if namespace.is_empty() && local == "id" {
                    let old_id = self
                        .dom
                        .with_node(node, |node| node.get_attribute("id").map(str::to_string))
                        .flatten();
                    self.dom
                        .with_node_mut(node, |node| node.remove_attribute_ns(namespace, local));
                    self.dom.update_id_index(node, old_id.as_deref(), None);
                } else {
                    self.dom
                        .with_node_mut(node, |node| node.remove_attribute_ns(namespace, local));
                }
                Ok("true".to_string())
            }
            "set_inner_html" => {
                let target = self.resolve_handle(arg1)?;
                if target == self.dom.document() {
                    return Ok("false".to_string());
                }
                for child in self.dom.children(target) {
                    self.dom.detach(child);
                }
                if !arg2.is_empty() {
                    let context_name = self
                        .dom
                        .with_node(target, |node| match &node.data {
                            NodeData::Element { name, .. } => Some(name.clone()),
                            _ => None,
                        })
                        .flatten();
                    let fragment = match context_name {
                        Some(name) => parse_fragment_with_context(arg2, name),
                        None => parse_fragment(arg2),
                    };
                    let root = fragment.fragment_root();
                    self.dom.import_children_from(target, &fragment, root);
                }
                Ok("true".to_string())
            }
            "set_inner_html_context" | "set_fragment_html_executable" => {
                let target = self.resolve_handle(arg1)?;
                if target == self.dom.document() {
                    return Ok("false".to_string());
                }
                let (context, html) = fragment_context_and_html(arg2);
                for child in self.dom.children(target) {
                    self.dom.detach(child);
                }
                if !html.is_empty() {
                    let fragment = parse_fragment_with_context(html, context);
                    let root = fragment.fragment_root();
                    self.dom.import_children_from(target, &fragment, root);
                }
                Ok("true".to_string())
            }
            "set_text_content" => {
                let node = self.resolve_handle(arg1)?;
                self.dom.with_node_mut(node, |node| match &mut node.data {
                    NodeData::Text { contents } | NodeData::Comment { contents } => {
                        *contents = arg2.to_string();
                    }
                    NodeData::ProcessingInstruction { data, .. } => {
                        *data = arg2.to_string();
                    }
                    _ => {}
                });
                Ok("true".to_string())
            }
            "template_contents" => {
                let node = self.resolve_handle(arg1)?;
                let contents = self.dom.template_contents(node);
                self.expose_optional_node(contents)
            }
            "create_document_fragment" => {
                let node = self.dom.new_node(NodeData::Document);
                Ok(self.expose_node(node)?.to_string())
            }
            "clone_node" => {
                let source = self.resolve_handle(arg1)?;
                let clone = self.dom.clone_node(source, arg2 == "true");
                self.expose_optional_node(clone)
            }
            "create_element" => {
                let node = self.dom.new_node(NodeData::Element {
                    name: html5ever::QualName::new(
                        None,
                        html5ever::Namespace::from("http://www.w3.org/1999/xhtml"),
                        html5ever::LocalName::from(arg1),
                    ),
                    attrs: vec![],
                    template_contents: None,
                    mathml_annotation_xml_integration_point: false,
                });
                Ok(self.expose_node(node)?.to_string())
            }
            "create_element_ns" => {
                let (namespace, qualified) = arg1.split_once('\0').unwrap_or(("", arg1));
                let (prefix, local) = match qualified.split_once(':') {
                    Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => {
                        (Some(html5ever::Prefix::from(prefix)), local)
                    }
                    None if !qualified.is_empty() => (None, qualified),
                    _ => return Ok("-1".to_string()),
                };
                let node = self.dom.new_node(NodeData::Element {
                    name: html5ever::QualName::new(
                        prefix,
                        html5ever::Namespace::from(namespace),
                        html5ever::LocalName::from(local),
                    ),
                    attrs: vec![],
                    template_contents: None,
                    mathml_annotation_xml_integration_point: false,
                });
                Ok(self.expose_node(node)?.to_string())
            }
            "create_text_node" => {
                let node = self.dom.new_node(NodeData::Text {
                    contents: arg1.to_string(),
                });
                Ok(self.expose_node(node)?.to_string())
            }
            "create_comment_node" => {
                let node = self.dom.new_node(NodeData::Comment {
                    contents: arg1.to_string(),
                });
                Ok(self.expose_node(node)?.to_string())
            }
            "create_processing_instruction" => {
                let node = self.dom.new_node(NodeData::ProcessingInstruction {
                    target: arg1.to_string(),
                    data: arg2.to_string(),
                });
                Ok(self.expose_node(node)?.to_string())
            }
            "create_doctype" => {
                let node = self.dom.new_node(NodeData::Doctype {
                    name: arg1.to_string(),
                    public_id: arg2.to_string(),
                    system_id: String::new(),
                });
                Ok(self.expose_node(node)?.to_string())
            }
            "pi_target" => {
                let node = self.resolve_handle(arg1)?;
                let target = self
                    .dom
                    .with_node(node, |node| match &node.data {
                        NodeData::ProcessingInstruction { target, .. } => Some(target.clone()),
                        _ => None,
                    })
                    .flatten()
                    .unwrap_or_default();
                serde_json::to_string(&target).map_err(|error| error.to_string())
            }
            "doctype_name" | "doctype_public_id" => {
                let node = self.resolve_handle(arg1)?;
                let value = self
                    .dom
                    .with_node(node, |node| match (&node.data, cmd) {
                        (NodeData::Doctype { name, .. }, "doctype_name") => Some(name.clone()),
                        (NodeData::Doctype { public_id, .. }, "doctype_public_id") => {
                            Some(public_id.clone())
                        }
                        _ => None,
                    })
                    .flatten()
                    .unwrap_or_default();
                serde_json::to_string(&value).map_err(|error| error.to_string())
            }
            "element_children" => {
                let node = self.resolve_handle(arg1)?;
                let children = self
                    .dom
                    .children(node)
                    .into_iter()
                    .filter(|child| {
                        self.dom
                            .get_node(*child)
                            .is_some_and(|node| node.is_element())
                    })
                    .collect();
                self.expose_nodes_json(children)
            }
            "has_child_nodes" => {
                let node = self.resolve_handle(arg1)?;
                Ok(self
                    .dom
                    .with_node(node, |node| node.first_child.is_some())
                    .unwrap_or(false)
                    .to_string())
            }
            "contains" => {
                let node = self.resolve_handle(arg1)?;
                let other = self.resolve_handle(arg2)?;
                Ok(self.dom.descendants(node).contains(&other).to_string())
            }
            "is_connected" => {
                let node = self.resolve_handle(arg1)?;
                Ok(self.dom.is_connected(node).to_string())
            }
            "node_index" => {
                let node = self.resolve_handle(arg1)?;
                Ok(node_child_index(&self.dom, node).to_string())
            }
            "compare_order" => {
                let first = self.resolve_handle(arg1)?;
                let second = self.resolve_handle(arg2)?;
                Ok(compare_node_order(&self.dom, first, second).to_string())
            }
            "node_root" => {
                let mut node = self.resolve_handle(arg1)?;
                let max_steps = self.dom.len().saturating_add(1);
                for _ in 0..max_steps {
                    match self.dom.with_node(node, |node| node.parent).flatten() {
                        Some(parent) => node = parent,
                        None => return Ok(self.expose_node(node)?.to_string()),
                    }
                }
                Err("DOM parent chain exceeds the tree-size safety bound".to_string())
            }
            _ => Ok("null".to_string()),
        }
    }
}

fn fragment_context_and_html(arg: &str) -> (html5ever::QualName, &str) {
    let mut parts = arg.splitn(3, '\0');
    let first = parts.next().unwrap_or("body");
    let second = parts.next();
    let third = parts.next();
    let (namespace, qualified, html) = match (second, third) {
        (Some(qualified), Some(html)) => (first, qualified, html),
        (Some(html), None) => ("http://www.w3.org/1999/xhtml", first, html),
        (None, None) => ("http://www.w3.org/1999/xhtml", "body", first),
        (None, Some(_)) => unreachable!(),
    };
    let (prefix, local) = match qualified.split_once(':') {
        Some((prefix, local)) if !prefix.is_empty() && !local.is_empty() => {
            (Some(html5ever::Prefix::from(prefix)), local)
        }
        _ => (
            None,
            if qualified.is_empty() {
                "body"
            } else {
                qualified
            },
        ),
    };
    (
        html5ever::QualName::new(
            prefix,
            html5ever::Namespace::from(namespace),
            html5ever::LocalName::from(local),
        ),
        html,
    )
}

fn is_dom_mutation_command(cmd: &str) -> bool {
    matches!(
        cmd,
        "set_attribute"
            | "append_child"
            | "remove_child"
            | "insert_before"
            | "remove_attribute"
            | "set_attribute_ns"
            | "remove_attribute_ns"
            | "set_inner_html"
            | "set_inner_html_context"
            | "set_fragment_html_executable"
            | "set_text_content"
            | "template_contents"
            | "create_document_fragment"
            | "clone_node"
            | "create_element"
            | "create_element_ns"
            | "create_text_node"
            | "create_comment_node"
            | "create_processing_instruction"
            | "create_doctype"
    )
}

fn node_child_index(dom: &DomTree, node: NodeId) -> usize {
    let mut index = 0usize;
    let mut current = dom.with_node(node, |node| node.prev_sibling).flatten();
    let max_steps = dom.len().saturating_add(1);
    for _ in 0..max_steps {
        match current {
            Some(previous) => {
                index += 1;
                current = dom.with_node(previous, |node| node.prev_sibling).flatten();
            }
            None => break,
        }
    }
    index
}

fn node_ancestors_root_first(dom: &DomTree, node: NodeId) -> Vec<NodeId> {
    let mut ancestors = vec![node];
    let mut current = node;
    let max_steps = dom.len().saturating_add(1);
    for _ in 0..max_steps {
        match dom.with_node(current, |node| node.parent).flatten() {
            Some(parent) => {
                ancestors.push(parent);
                current = parent;
            }
            None => break,
        }
    }
    ancestors.reverse();
    ancestors
}

fn compare_node_order(dom: &DomTree, first: NodeId, second: NodeId) -> i32 {
    if first == second {
        return 0;
    }
    let first_ancestors = node_ancestors_root_first(dom, first);
    let second_ancestors = node_ancestors_root_first(dom, second);
    if first_ancestors.first() != second_ancestors.first() {
        return if first.index() < second.index() {
            -1
        } else {
            1
        };
    }
    let mut index = 0usize;
    while index < first_ancestors.len()
        && index < second_ancestors.len()
        && first_ancestors[index] == second_ancestors[index]
    {
        index += 1;
    }
    if index >= first_ancestors.len() {
        return -1;
    }
    if index >= second_ancestors.len() {
        return 1;
    }
    if node_child_index(dom, first_ancestors[index])
        < node_child_index(dom, second_ancestors[index])
    {
        -1
    } else {
        1
    }
}

#[wasm_bindgen]
pub fn version() -> String {
    boundary_value("version", || env!("CARGO_PKG_VERSION").to_string())
}

/// Monotonic version for the JavaScript/WASM ownership and serialization ABI.
#[wasm_bindgen]
pub fn abi_version() -> u32 {
    ABI_VERSION
}

/// Machine-readable capability probe used by the Node Worker harness.
#[wasm_bindgen]
pub fn probe() -> String {
    boundary_value("probe", || {
        format!(
            r#"{{"abiVersion":{},"dom":true,"selectors":true,"javascript":"host","embeddedV8":false,"domOpAbiVersion":{},"domBatchAbiVersion":{},"documentMetadataAbiVersion":1,"stableNodeHandles":true}}"#,
            ABI_VERSION, DOM_OP_ABI_VERSION, DOM_BATCH_ABI_VERSION
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(core: &mut ObscuraCore, cmd: &str, arg1: &str, arg2: &str) -> String {
        core.dom_op_inner(cmd, arg1, arg2).unwrap()
    }

    fn handle(core: &mut ObscuraCore, selector: &str) -> String {
        let value = op(core, "query_selector", selector, "");
        assert_ne!(value, "-1", "selector {selector:?} did not match");
        value
    }

    #[test]
    fn parses_and_queries_with_the_existing_dom_engine() {
        let core = ObscuraCore::new(
            "<!doctype html><main><h1 class='title'>Obscura</h1><p>portable</p></main>",
        )
        .unwrap();
        assert_eq!(core.query_count("main > *").unwrap(), 2);
        assert_eq!(
            core.query_text(".title").unwrap().as_deref(),
            Some("Obscura")
        );
        assert!(core
            .query_html("main")
            .unwrap()
            .unwrap()
            .contains("portable"));
        let document_element = core.document_element_html().unwrap();
        assert!(document_element.starts_with("<html"));
        assert!(!document_element.to_ascii_lowercase().contains("<!doctype"));
        assert_eq!(abi_version(), 1);
        assert!(probe().contains(r#""abiVersion":1"#));
        assert!(probe().contains(r#""domOpAbiVersion":1"#));
        assert!(probe().contains(r#""domBatchAbiVersion":1"#));
        assert!(probe().contains(r#""stableNodeHandles":true"#));
    }

    #[test]
    fn op_dom_compatible_surface_covers_identity_queries_mutation_and_creation() {
        let mut core = ObscuraCore::new(
            "<!doctype html><html><head><title> Obscura\n Test </title></head><body>\
             <main id='main'><h1 data-x='a'>Hello</h1><p>World</p>\
             <template><b>T</b></template></main></body></html>",
        )
        .unwrap();
        core.set_document_metadata("https://example.test/a", "https://ref.test/", "UTF-8")
            .unwrap();

        let document = op(&mut core, "document_node_id", "", "");
        let html = op(&mut core, "document_element", "", "");
        let main = op(&mut core, "get_element_by_id", "main", "");
        let h1 = handle(&mut core, "h1");
        let paragraph = handle(&mut core, "p");
        let template = handle(&mut core, "template");

        assert_eq!(document, core.document_handle().to_string());
        assert_eq!(op(&mut core, "document_title", "", ""), r#""Obscura Test""#);
        assert_eq!(
            op(&mut core, "document_url", "", ""),
            r#""https://example.test/a""#
        );
        assert_eq!(
            op(&mut core, "document_referrer", "", ""),
            r#""https://ref.test/""#
        );
        assert_eq!(op(&mut core, "document_encoding", "", ""), r#""UTF-8""#);
        let doctype: serde_json::Value =
            serde_json::from_str(&op(&mut core, "document_doctype", "", "")).unwrap();
        assert_eq!(doctype["name"], "html");
        let doctype_handle = doctype["nodeId"].as_u64().unwrap().to_string();

        assert_eq!(op(&mut core, "node_type", &document, ""), "9");
        assert_eq!(op(&mut core, "node_name", &document, ""), "\"#document\"");
        assert_eq!(op(&mut core, "node_type", &h1, ""), "1");
        assert_eq!(op(&mut core, "node_name", &h1, ""), r#""H1""#);
        assert_eq!(op(&mut core, "tag_name", &h1, ""), r#""H1""#);
        assert_eq!(op(&mut core, "local_name", &h1, ""), r#""h1""#);
        assert_eq!(
            op(&mut core, "namespace_uri", &h1, ""),
            r#""http://www.w3.org/1999/xhtml""#
        );
        assert_eq!(op(&mut core, "text_content", &h1, ""), r#""Hello""#);
        assert_eq!(op(&mut core, "parent_node", &h1, ""), main);
        assert_ne!(op(&mut core, "first_child", &h1, ""), "-1");
        assert_ne!(op(&mut core, "last_child", &main, ""), "-1");
        assert_eq!(op(&mut core, "next_sibling", &h1, ""), paragraph);
        assert_eq!(op(&mut core, "prev_sibling", &paragraph, ""), h1);
        assert_ne!(op(&mut core, "next_in_subtree", &main, &h1), "-1");
        assert_ne!(op(&mut core, "prev_in_subtree", &main, &paragraph), "-1");
        assert_eq!(op(&mut core, "next_after_subtree", &main, &h1), paragraph);

        let child_nodes: Vec<u32> =
            serde_json::from_str(&op(&mut core, "child_nodes", &main, "")).unwrap();
        let element_children: Vec<u32> =
            serde_json::from_str(&op(&mut core, "element_children", &main, "")).unwrap();
        assert_eq!(child_nodes, element_children);
        assert_eq!(child_nodes.len(), 3);
        assert_eq!(op(&mut core, "has_child_nodes", &main, ""), "true");
        assert_eq!(op(&mut core, "contains", &main, &h1), "true");
        assert_eq!(op(&mut core, "is_connected", &h1, ""), "true");
        assert_eq!(op(&mut core, "node_index", &paragraph, ""), "1");
        assert_eq!(op(&mut core, "compare_order", &h1, &paragraph), "-1");
        assert_eq!(op(&mut core, "node_root", &h1, ""), document);

        assert_eq!(
            op(&mut core, "query_selector_scoped", &main, "p"),
            paragraph
        );
        let scoped: Vec<u32> =
            serde_json::from_str(&op(&mut core, "query_selector_all_scoped", &main, "h1, p"))
                .unwrap();
        assert_eq!(
            scoped,
            vec![
                h1.parse::<u32>().unwrap(),
                paragraph.parse::<u32>().unwrap()
            ]
        );
        assert_eq!(
            op(&mut core, "matches_selector", &h1, ".missing, h1"),
            "true"
        );
        let all: Vec<u32> =
            serde_json::from_str(&op(&mut core, "query_selector_all", "main > *", "")).unwrap();
        assert_eq!(all.len(), 3);

        assert_eq!(op(&mut core, "get_attribute", &h1, "data-x"), r#""a""#);
        assert_eq!(op(&mut core, "attribute_names", &h1, ""), r#"["data-x"]"#);
        assert_eq!(op(&mut core, "set_attribute", &h1, "class\0hero"), "true");
        assert_eq!(op(&mut core, "get_attribute", &h1, "class"), r#""hero""#);
        assert_eq!(op(&mut core, "remove_attribute", &h1, "class"), "true");
        assert_eq!(op(&mut core, "get_attribute", &h1, "class"), "null");
        assert_eq!(
            op(&mut core, "set_attribute_ns", &h1, "urn:test\0x:key\0value"),
            "true"
        );
        assert_eq!(
            op(&mut core, "get_attribute_ns", &h1, "urn:test\0key"),
            r#""value""#
        );
        assert_eq!(
            op(&mut core, "remove_attribute_ns", &h1, "urn:test\0key"),
            "true"
        );
        assert_eq!(op(&mut core, "inner_html", &h1, ""), "\"Hello\"");
        assert!(op(&mut core, "outer_html", &main, "").contains("<main"));

        let span = op(&mut core, "create_element", "span", "");
        let text = op(&mut core, "create_text_node", "new", "");
        assert_eq!(op(&mut core, "append_child", &span, &text), "true");
        assert_eq!(op(&mut core, "append_child", &main, &span), "true");
        let emphasis = op(&mut core, "create_element", "em", "");
        assert_eq!(op(&mut core, "insert_before", &emphasis, &span), "true");
        assert_eq!(op(&mut core, "remove_child", &emphasis, ""), "true");
        assert_eq!(op(&mut core, "node_type", &emphasis, ""), "1");
        // Reject insertion of an ancestor below its descendant without
        // corrupting the tree or invalidating the existing handles.
        assert_eq!(op(&mut core, "append_child", &h1, &main), "false");
        assert_eq!(op(&mut core, "parent_node", &h1, ""), main);

        let detached = op(&mut core, "create_element", "div", "");
        assert_eq!(
            op(&mut core, "set_inner_html", &detached, "<i>A</i>"),
            "true"
        );
        assert_ne!(op(&mut core, "query_selector_scoped", &detached, "i"), "-1");
        assert_eq!(
            op(
                &mut core,
                "set_inner_html_context",
                &detached,
                "http://www.w3.org/1999/xhtml\0div\0<b>B</b>",
            ),
            "true"
        );
        assert_ne!(op(&mut core, "query_selector_scoped", &detached, "b"), "-1");
        assert_eq!(
            op(
                &mut core,
                "set_fragment_html_executable",
                &detached,
                "div\0<script>1</script>",
            ),
            "true"
        );
        assert_ne!(
            op(&mut core, "query_selector_scoped", &detached, "script"),
            "-1"
        );
        assert_eq!(op(&mut core, "set_text_content", &text, "changed"), "true");
        assert_eq!(op(&mut core, "text_content", &text, ""), r#""changed""#);

        assert_ne!(op(&mut core, "template_contents", &template, ""), "-1");
        assert_ne!(op(&mut core, "create_document_fragment", "", ""), "-1");
        assert_ne!(op(&mut core, "clone_node", &main, "true"), "-1");
        let svg = op(
            &mut core,
            "create_element_ns",
            "http://www.w3.org/2000/svg\0svg",
            "",
        );
        assert_eq!(
            op(&mut core, "namespace_uri", &svg, ""),
            r#""http://www.w3.org/2000/svg""#
        );
        let comment = op(&mut core, "create_comment_node", "note", "");
        assert_eq!(op(&mut core, "node_type", &comment, ""), "8");
        let pi = op(&mut core, "create_processing_instruction", "xml", "data");
        assert_eq!(op(&mut core, "pi_target", &pi, ""), r#""xml""#);
        let created_doctype = op(&mut core, "create_doctype", "html", "public");
        assert_eq!(
            op(&mut core, "doctype_name", &created_doctype, ""),
            r#""html""#
        );
        assert_eq!(
            op(&mut core, "doctype_public_id", &created_doctype, ""),
            r#""public""#
        );
        assert_eq!(
            op(&mut core, "doctype_name", &doctype_handle, ""),
            r#""html""#
        );
        assert_eq!(op(&mut core, "unknown_command", "", ""), "null");
        assert!(core.page_revision() > 0);
        assert_ne!(html, main);
    }

    #[test]
    fn handles_are_stable_until_page_reset_and_never_alias_after_reset() {
        let mut core = ObscuraCore::new("<main><h1>old</h1></main>").unwrap();
        let old_document = core.document_handle();
        let old_heading = handle(&mut core, "h1");
        assert_eq!(handle(&mut core, "h1"), old_heading);

        let revision = core.page_revision();
        core.set_html("<main><h1>new</h1></main>").unwrap();
        assert_eq!(core.page_revision(), revision + 1);
        assert!(core.document_handle() > old_document);
        assert!(core
            .dom_op_inner("node_type", &old_heading, "")
            .unwrap_err()
            .contains("stale or unknown node handle"));
        let new_heading = handle(&mut core, "h1");
        assert!(new_heading.parse::<u32>().unwrap() > old_heading.parse::<u32>().unwrap());
        assert_eq!(op(&mut core, "text_content", &new_heading, ""), r#""new""#);
    }

    #[test]
    fn batches_are_bounded_prevalidated_ordered_and_explicitly_non_transactional() {
        let mut core = ObscuraCore::new("<main><h1>value</h1></main>").unwrap();
        let heading = handle(&mut core, "h1");
        let revision = core.page_revision();
        let request = serde_json::json!([
            ["set_attribute", heading, "class\0hero"],
            ["get_attribute", heading, "class"],
            ["text_content", heading, ""],
        ])
        .to_string();
        let results: Vec<String> =
            serde_json::from_str(&core.dom_batch_inner(&request).unwrap()).unwrap();
        assert_eq!(results, vec!["true", r#""hero""#, r#""value""#]);
        assert_eq!(core.page_revision(), revision + 1);

        // Shape validation completes before execution, so malformed envelopes
        // cannot apply their valid prefix.
        let malformed = format!(
            r#"[["set_attribute","{}","id\u0000changed"],["node_type"]]"#,
            heading
        );
        assert!(core.dom_batch_inner(&malformed).is_err());
        assert_eq!(op(&mut core, "get_attribute", &heading, "id"), "null");

        // Semantic failures are ordered and non-transactional by contract: a
        // completed prefix remains visible when a later stale handle rejects.
        let partial = format!(
            r#"[["set_attribute","{}","id\u0000kept"],["node_type","4294967294",""]]"#,
            heading
        );
        assert!(core.dom_batch_inner(&partial).is_err());
        assert_eq!(op(&mut core, "get_attribute", &heading, "id"), r#""kept""#);

        let too_many = serde_json::Value::Array(
            (0..=MAX_DOM_BATCH_OPS)
                .map(|_| serde_json::json!(["document_title", "", ""]))
                .collect(),
        )
        .to_string();
        assert!(core
            .dom_batch_inner(&too_many)
            .unwrap_err()
            .contains("operation ABI limit"));
        assert!(ObscuraCore::validate_dom_args(
            "query_selector",
            &"x".repeat(MAX_SELECTOR_BYTES + 1),
            ""
        )
        .is_err());
    }
}
