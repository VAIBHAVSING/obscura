//! Transport-free browser/context/target/session state for portable CDP.
//!
//! This module is intentionally independent of Tokio, sockets, V8 and the
//! native `Page` type.  It is the state layer that both the native dispatcher
//! and the future WASM dispatcher can use while a host remains responsible for
//! transport and asynchronous actions.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::{
    ActionId, ActionResult, CdpEngine, CdpFailure, ContextId, ContextOptions, EngineAction,
    PageId, PageSnapshot,
};
use crate::protocol::MAX_METHOD_BYTES;

pub const MAX_CONTEXTS: usize = 256;
pub const MAX_PAGES: usize = 4096;
pub const MAX_CONNECTIONS: usize = 512;
pub const MAX_SESSIONS: usize = 8192;
pub const MAX_ACTIONS: usize = 512;
pub const MAX_URL_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_STORAGE_STATE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_NETWORK_RESPONSE_BODIES: usize = 128;
pub const MAX_NETWORK_RESPONSE_BODY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_NETWORK_RESPONSE_WIRE_BYTES: usize = MAX_NETWORK_RESPONSE_BODY_BYTES * 2;
pub const MAX_NETWORK_REQUEST_ID_BYTES: usize = 256;
pub const MAX_NETWORK_HEADERS: usize = 256;
pub const MAX_NETWORK_HEADER_BYTES: usize = 64 * 1024;
pub const MAX_FETCH_PATTERNS: usize = 64;
pub const MAX_FETCH_PATTERN_BYTES: usize = 2048;
pub const MAX_FETCH_REQUESTS: usize = 256;
pub const MAX_FETCH_RESOLUTIONS: usize = 256;
pub const DEFAULT_VIEWPORT_WIDTH: u32 = 800;
pub const DEFAULT_VIEWPORT_HEIGHT: u32 = 600;
pub const MAX_VIEWPORT_DIMENSION: u32 = 4096;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ConnectionId(u64);

impl ConnectionId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for ConnectionId {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl From<ConnectionId> for u64 {
    fn from(value: ConnectionId) -> Self {
        value.get()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SessionId(u64);

impl SessionId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for SessionId {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl From<SessionId> for u64 {
    fn from(value: SessionId) -> Self {
        value.get()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextState {
    pub id: ContextId,
    pub options: ContextOptions,
    pub generation: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PageState {
    pub id: PageId,
    pub context_id: ContextId,
    pub url: String,
    pub title: String,
    pub frame_id: String,
    pub loader_id: String,
    pub document_generation: u64,
    pub network: PageNetworkState,
}

/// Per-page Network policy and response-body state shared by CDP adapters.
///
/// The host may own the actual transport/cache bytes, but the portable core
/// owns which sessions are enabled, request headers, cache policy, and the
/// bounded CDP response-body view.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PageNetworkState {
    pub enabled_sessions: BTreeSet<SessionId>,
    pub cache_disabled: bool,
    pub extra_headers: BTreeMap<String, String>,
    pub response_bodies: BTreeMap<String, NetworkResponseBody>,
    pub fetch_patterns: BTreeMap<SessionId, Vec<FetchPatternState>>,
    pub paused_requests: BTreeSet<String>,
    response_body_order: BTreeMap<u64, String>,
    response_body_bytes: usize,
    next_response_order: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NetworkResponseBody {
    pub body: String,
    pub base64_encoded: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FetchPatternState {
    pub url_pattern: String,
    pub request_stage: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PageDisplayState {
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: f64,
    pub mobile: bool,
    pub emulated_media: String,
    pub focus_emulation: bool,
}

impl Default for PageDisplayState {
    fn default() -> Self {
        Self {
            width: DEFAULT_VIEWPORT_WIDTH,
            height: DEFAULT_VIEWPORT_HEIGHT,
            device_scale_factor: 1.0,
            mobile: false,
            emulated_media: String::new(),
            focus_emulation: false,
        }
    }
}

impl PageNetworkState {
    fn clear_response_bodies(&mut self) {
        self.response_bodies.clear();
        self.response_body_order.clear();
        self.response_body_bytes = 0;
    }

    fn remove_response_body(&mut self, request_id: &str) {
        if let Some(body) = self.response_bodies.remove(request_id) {
            self.response_body_bytes = self.response_body_bytes.saturating_sub(body.body.len());
        }
        self.response_body_order.retain(|_, value| value != request_id);
    }

    fn evict_oldest_response_body(&mut self) {
        let Some((order, request_id)) = self
            .response_body_order
            .iter()
            .next()
            .map(|(order, request_id)| (*order, request_id.clone()))
        else {
            return;
        };
        self.response_body_order.remove(&order);
        if let Some(body) = self.response_bodies.remove(&request_id) {
            self.response_body_bytes = self.response_body_bytes.saturating_sub(body.body.len());
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct PendingAction {
    action: EngineAction,
    page: Option<PageId>,
    context: Option<ContextId>,
    generation: u64,
}

/// Bounded browser identity state shared by transport adapters.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BrowserState {
    contexts: BTreeMap<ContextId, ContextState>,
    pages: BTreeMap<PageId, PageState>,
    display: BTreeMap<PageId, PageDisplayState>,
    fetch_resolutions: BTreeMap<PageId, VecDeque<Value>>,
    connections: BTreeSet<ConnectionId>,
    sessions: BTreeMap<SessionId, PageId>,
    session_connections: BTreeMap<SessionId, ConnectionId>,
    actions: BTreeMap<ActionId, PendingAction>,
    next_context: u64,
    next_page: u64,
    next_connection: u64,
    next_session: u64,
    next_action: u64,
    generation: u64,
    closed: bool,
}

impl Default for BrowserState {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserState {
    /// Construct a live state with one default browser context.
    pub fn new() -> Self {
        let default_id = ContextId::new(1);
        let mut contexts = BTreeMap::new();
        contexts.insert(
            default_id,
            ContextState {
                id: default_id,
                options: ContextOptions::default(),
                generation: 1,
            },
        );
        Self {
            contexts,
            pages: BTreeMap::new(),
            display: BTreeMap::new(),
            fetch_resolutions: BTreeMap::new(),
            connections: BTreeSet::new(),
            sessions: BTreeMap::new(),
            session_connections: BTreeMap::new(),
            actions: BTreeMap::new(),
            next_context: 1,
            next_page: 0,
            next_connection: 0,
            next_session: 0,
            next_action: 0,
            generation: 1,
            closed: false,
        }
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn default_context(&self) -> ContextId {
        ContextId::new(1)
    }

    pub fn contexts(&self) -> impl Iterator<Item = &ContextState> {
        self.contexts.values()
    }

    pub fn pages(&self) -> impl Iterator<Item = &PageState> {
        self.pages.values()
    }

    pub fn context(&self, id: &ContextId) -> Option<&ContextState> {
        self.contexts.get(id)
    }

    pub fn page(&self, id: &PageId) -> Option<&PageState> {
        self.pages.get(id)
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn pending_action_count(&self) -> usize {
        self.actions.len()
    }

    pub fn network_state(&self, id: &PageId) -> Option<&PageNetworkState> {
        self.pages.get(id).map(|page| &page.network)
    }

    pub fn display_state(&self, id: &PageId) -> Option<&PageDisplayState> {
        self.display.get(id)
    }

    pub fn context_for_page(&self, id: &PageId) -> Result<ContextId, CdpFailure> {
        self.pages
            .get(id)
            .map(|page| page.context_id)
            .ok_or(CdpFailure::UnknownPage(*id))
    }

    pub fn set_device_metrics(
        &mut self,
        page: &PageId,
        width: u32,
        height: u32,
        device_scale_factor: f64,
        mobile: bool,
    ) -> Result<(), CdpFailure> {
        if width == 0
            || height == 0
            || width > MAX_VIEWPORT_DIMENSION
            || height > MAX_VIEWPORT_DIMENSION
            || !device_scale_factor.is_finite()
            || !(0.0..=8.0).contains(&device_scale_factor)
        {
            return Err(CdpFailure::invalid_argument("device metrics are outside portable limits"));
        }
        let display = self.display.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        display.width = width;
        display.height = height;
        display.device_scale_factor = device_scale_factor;
        display.mobile = mobile;
        Ok(())
    }

    pub fn clear_device_metrics(&mut self, page: &PageId) -> Result<(), CdpFailure> {
        let display = self.display.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        display.width = DEFAULT_VIEWPORT_WIDTH;
        display.height = DEFAULT_VIEWPORT_HEIGHT;
        display.device_scale_factor = 1.0;
        display.mobile = false;
        Ok(())
    }

    pub fn set_emulated_media(&mut self, page: &PageId, media: String) -> Result<(), CdpFailure> {
        if media.len() > MAX_METHOD_BYTES {
            return Err(CdpFailure::invalid_argument("emulated media exceeds the byte limit"));
        }
        let display = self.display.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        display.emulated_media = media;
        Ok(())
    }

    pub fn set_focus_emulation(&mut self, page: &PageId, enabled: bool) -> Result<(), CdpFailure> {
        let display = self.display.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        display.focus_emulation = enabled;
        Ok(())
    }

    pub fn network_enable(&mut self, page: &PageId, session: SessionId) -> Result<(), CdpFailure> {
        let page = self.pages.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        if page.network.enabled_sessions.len() >= MAX_SESSIONS
            && !page.network.enabled_sessions.contains(&session)
        {
            return Err(CdpFailure::ActionQueueFull);
        }
        page.network.enabled_sessions.insert(session);
        Ok(())
    }

    pub fn network_disable(
        &mut self,
        page: &PageId,
        session: Option<SessionId>,
    ) -> Result<(), CdpFailure> {
        let page = self.pages.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        if let Some(session) = session {
            page.network.enabled_sessions.remove(&session);
        } else {
            page.network.enabled_sessions.clear();
        }
        page.network.clear_response_bodies();
        Ok(())
    }

    pub fn set_cache_disabled(&mut self, page: &PageId, disabled: bool) -> Result<(), CdpFailure> {
        let page = self.pages.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        page.network.cache_disabled = disabled;
        Ok(())
    }

    pub fn cache_disabled(&self, page: &PageId) -> Result<bool, CdpFailure> {
        self.pages
            .get(page)
            .map(|page| page.network.cache_disabled)
            .ok_or(CdpFailure::UnknownPage(*page))
    }

    pub fn set_extra_headers(
        &mut self,
        page: &PageId,
        headers: BTreeMap<String, String>,
    ) -> Result<(), CdpFailure> {
        if headers.len() > MAX_NETWORK_HEADERS {
            return Err(CdpFailure::invalid_argument("Network headers exceed the entry limit"));
        }
        let bytes = headers
            .iter()
            .map(|(name, value)| name.len().saturating_add(value.len()))
            .sum::<usize>();
        if bytes > MAX_NETWORK_HEADER_BYTES {
            return Err(CdpFailure::invalid_argument("Network headers exceed the byte limit"));
        }
        if headers.keys().any(|name| name.is_empty() || name.len() > MAX_METHOD_BYTES)
            || headers.values().any(|value| value.len() > MAX_NETWORK_HEADER_BYTES)
        {
            return Err(CdpFailure::invalid_argument("Network header name or value is invalid"));
        }
        let page = self.pages.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        page.network.extra_headers = headers;
        Ok(())
    }

    pub fn extra_headers(&self, page: &PageId) -> Result<BTreeMap<String, String>, CdpFailure> {
        self.pages
            .get(page)
            .map(|page| page.network.extra_headers.clone())
            .ok_or(CdpFailure::UnknownPage(*page))
    }

    pub fn store_response_body(
        &mut self,
        page: &PageId,
        request_id: &str,
        body: String,
        base64_encoded: bool,
    ) -> Result<(), CdpFailure> {
        if request_id.is_empty() || request_id.len() > MAX_NETWORK_REQUEST_ID_BYTES {
            return Err(CdpFailure::invalid_argument("Network requestId is invalid"));
        }
        let wire_bytes = body.len();
        if wire_bytes > MAX_NETWORK_RESPONSE_WIRE_BYTES {
            return Err(CdpFailure::invalid_argument("Network response body exceeds the byte limit"));
        }
        let page = self.pages.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        page.network.remove_response_body(request_id);
        while (page.network.response_bodies.len() >= MAX_NETWORK_RESPONSE_BODIES
            || page.network.response_body_bytes.saturating_add(wire_bytes) > MAX_NETWORK_RESPONSE_WIRE_BYTES)
            && !page.network.response_bodies.is_empty()
        {
            page.network.evict_oldest_response_body();
        }
        let order = page.network.next_response_order;
        page.network.next_response_order = page
            .network
            .next_response_order
            .checked_add(1)
            .ok_or(CdpFailure::IdExhausted)?;
        page.network.response_body_order.insert(order, request_id.to_string());
        page.network.response_body_bytes = page.network.response_body_bytes.saturating_add(wire_bytes);
        page.network.response_bodies.insert(
            request_id.to_string(),
            NetworkResponseBody { body, base64_encoded },
        );
        Ok(())
    }

    pub fn response_body(&self, page: &PageId, request_id: &str) -> Result<NetworkResponseBody, CdpFailure> {
        self.pages
            .get(page)
            .and_then(|page| page.network.response_bodies.get(request_id))
            .cloned()
            .ok_or_else(|| CdpFailure::UnknownPage(*page))
    }

    pub fn clear_response_bodies(&mut self, page: &PageId) -> Result<(), CdpFailure> {
        let page = self.pages.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        page.network.clear_response_bodies();
        Ok(())
    }

    pub fn set_fetch_patterns(
        &mut self,
        page: &PageId,
        session: SessionId,
        patterns: Vec<FetchPatternState>,
    ) -> Result<(), CdpFailure> {
        if patterns.is_empty() || patterns.len() > MAX_FETCH_PATTERNS {
            return Err(CdpFailure::invalid_argument("Fetch patterns exceed the item limit"));
        }
        if patterns.iter().any(|pattern| {
            pattern.url_pattern.len() > MAX_FETCH_PATTERN_BYTES
                || !matches!(pattern.request_stage.as_str(), "Request" | "Response")
        }) {
            return Err(CdpFailure::invalid_argument("Fetch pattern is invalid"));
        }
        let page = self.pages.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        page.network.fetch_patterns.insert(session, patterns);
        Ok(())
    }

    pub fn fetch_patterns(&self, page: &PageId, session: SessionId) -> Option<&[FetchPatternState]> {
        self.pages
            .get(page)
            .and_then(|page| page.network.fetch_patterns.get(&session))
            .map(Vec::as_slice)
    }

    pub fn clear_fetch_patterns(&mut self, page: &PageId, session: SessionId) -> Result<(), CdpFailure> {
        let continue_all = {
            let page_state = self.pages.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
            page_state.network.fetch_patterns.remove(&session);
            page_state.network.fetch_patterns.is_empty()
        };
        if !continue_all {
            return Ok(());
        }
        let paused: Vec<String> = self
            .pages
            .get(page)
            .map(|page| page.network.paused_requests.iter().cloned().collect())
            .unwrap_or_default();
        for request_id in paused {
            self.queue_fetch_resolution(
                page,
                request_id.clone(),
                serde_json::json!({"requestId": request_id, "action": "continue"}),
            )?;
        }
        Ok(())
    }

    pub fn register_paused_request(&mut self, page: &PageId, request_id: &str) -> Result<(), CdpFailure> {
        if request_id.is_empty() || request_id.len() > MAX_NETWORK_REQUEST_ID_BYTES {
            return Err(CdpFailure::invalid_argument("Fetch requestId is invalid"));
        }
        let page = self.pages.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        if page.network.paused_requests.len() >= MAX_FETCH_REQUESTS
            && !page.network.paused_requests.contains(request_id)
        {
            return Err(CdpFailure::ActionQueueFull);
        }
        page.network.paused_requests.insert(request_id.to_string());
        Ok(())
    }

    pub fn cancel_paused_request(&mut self, page: &PageId, request_id: &str) -> Result<(), CdpFailure> {
        let page = self.pages.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
        page.network.paused_requests.remove(request_id);
        Ok(())
    }

    pub fn queue_fetch_resolution(
        &mut self,
        page: &PageId,
        request_id: String,
        resolution: Value,
    ) -> Result<bool, CdpFailure> {
        let was_paused = {
            let page_state = self.pages.get_mut(page).ok_or(CdpFailure::UnknownPage(*page))?;
            page_state.network.paused_requests.remove(&request_id)
        };
        if !was_paused {
            return Ok(false);
        }
        let queue = self.fetch_resolutions.entry(*page).or_default();
        if queue.len() >= MAX_FETCH_RESOLUTIONS {
            if let Some(page_state) = self.pages.get_mut(page) {
                page_state.network.paused_requests.insert(request_id);
            }
            return Err(CdpFailure::ActionQueueFull);
        }
        queue.push_back(resolution);
        Ok(true)
    }

    pub fn drain_fetch_resolutions(&mut self, page: &PageId, max_items: usize) -> Vec<Value> {
        let Some(queue) = self.fetch_resolutions.get_mut(page) else {
            return Vec::new();
        };
        let count = max_items.min(queue.len());
        let values: Vec<Value> = queue.drain(..count).collect();
        if queue.is_empty() {
            self.fetch_resolutions.remove(page);
        }
        values
    }

    pub fn fetch_resolution_count(&self) -> usize {
        self.fetch_resolutions.values().map(VecDeque::len).sum()
    }

    /// Update target metadata after a host navigation or document commit.
    /// The identity remains stable while the document generation advances.
    pub fn update_page(
        &mut self,
        id: &PageId,
        url: Option<&str>,
        title: Option<&str>,
        loader_id: Option<&str>,
        document_generation: Option<u64>,
    ) -> Result<(), CdpFailure> {
        if let Some(url) = url {
            Self::validate_url(url)?;
        }
        let page = self.pages.get_mut(id).ok_or(CdpFailure::UnknownPage(*id))?;
        if let Some(url) = url {
            page.url = url.to_string();
        }
        if let Some(title) = title {
            page.title = title.to_string();
        }
        if let Some(loader_id) = loader_id {
            page.loader_id = loader_id.to_string();
        }
        if let Some(document_generation) = document_generation {
            page.document_generation = document_generation;
        }
        Ok(())
    }

    pub fn open_connection(&mut self) -> Result<ConnectionId, CdpFailure> {
        self.ensure_open()?;
        if self.connections.len() >= MAX_CONNECTIONS {
            return Err(CdpFailure::ActionQueueFull);
        }
        let id = Self::next_id(&mut self.next_connection)?;
        let id = ConnectionId::new(id);
        self.connections.insert(id);
        Ok(id)
    }

    pub fn close_connection(&mut self, id: ConnectionId) -> bool {
        let removed = self.connections.remove(&id);
        if removed {
            let sessions: Vec<SessionId> = self
                .session_connections
                .iter()
                .filter_map(|(session, connection)| (*connection == id).then_some(*session))
                .collect();
            for session in sessions {
                let _ = self.detach(session);
            }
        }
        removed
    }

    pub fn attach(&mut self, connection: ConnectionId, page: PageId) -> Result<SessionId, CdpFailure> {
        self.ensure_open()?;
        if !self.connections.contains(&connection) {
            return Err(CdpFailure::InvalidArgument("connection is not open".to_string()));
        }
        if !self.pages.contains_key(&page) {
            return Err(CdpFailure::UnknownPage(page));
        }
        if self.sessions.len() >= MAX_SESSIONS {
            return Err(CdpFailure::ActionQueueFull);
        }
        let id = SessionId::new(Self::next_id(&mut self.next_session)?);
        self.sessions.insert(id, page);
        self.session_connections.insert(id, connection);
        Ok(id)
    }

    pub fn detach(&mut self, id: SessionId) -> Option<PageId> {
        self.session_connections.remove(&id);
        let page = self.sessions.remove(&id);
        if let Some(page_id) = page {
            if let Some(page) = self.pages.get_mut(&page_id) {
                page.network.enabled_sessions.remove(&id);
            }
            let _ = self.clear_fetch_patterns(&page_id, id);
        }
        page
    }

    pub fn attached_page(&self, id: SessionId) -> Option<PageId> {
        self.sessions.get(&id).copied()
    }

    pub fn page_is_attached(&self, id: PageId) -> bool {
        self.sessions.values().any(|page| *page == id)
    }

    pub fn close(&mut self) {
        self.closed = true;
        self.contexts.clear();
        self.pages.clear();
        self.display.clear();
        self.fetch_resolutions.clear();
        self.connections.clear();
        self.sessions.clear();
        self.session_connections.clear();
        self.actions.clear();
        self.generation = self.generation.saturating_add(1);
    }

    fn ensure_open(&self) -> Result<(), CdpFailure> {
        if self.closed {
            Err(CdpFailure::Closed)
        } else {
            Ok(())
        }
    }

    fn next_id(counter: &mut u64) -> Result<u64, CdpFailure> {
        let next = counter.checked_add(1).ok_or(CdpFailure::IdExhausted)?;
        *counter = next;
        Ok(next)
    }

    fn validate_url(url: &str) -> Result<(), CdpFailure> {
        if url.len() > MAX_URL_BYTES {
            Err(CdpFailure::invalid_argument("page URL exceeds the byte limit"))
        } else {
            Ok(())
        }
    }

    fn validate_options(options: &ContextOptions) -> Result<(), CdpFailure> {
        if options
            .storage_state
            .as_ref()
            .is_some_and(|state| state.len() > MAX_STORAGE_STATE_BYTES)
        {
            return Err(CdpFailure::invalid_argument("context storage state exceeds the byte limit"));
        }
        Ok(())
    }

    fn remove_page_state(&mut self, id: PageId) -> Result<(), CdpFailure> {
        if self.pages.remove(&id).is_none() {
            return Err(CdpFailure::UnknownPage(id));
        }
        self.display.remove(&id);
        self.fetch_resolutions.remove(&id);
        self.sessions.retain(|_, page| *page != id);
        let live_sessions: BTreeSet<SessionId> = self.sessions.keys().copied().collect();
        self.session_connections.retain(|session, _| live_sessions.contains(session));
        self.actions.retain(|_, pending| pending.page != Some(id));
        Ok(())
    }
}

impl CdpEngine for BrowserState {
    fn create_context(&mut self, options: ContextOptions) -> Result<ContextId, CdpFailure> {
        self.ensure_open()?;
        Self::validate_options(&options)?;
        if self.contexts.len() >= MAX_CONTEXTS {
            return Err(CdpFailure::ActionQueueFull);
        }
        let id = ContextId::new(Self::next_id(&mut self.next_context)?);
        self.contexts.insert(
            id,
            ContextState {
                id,
                options,
                generation: self.generation,
            },
        );
        Ok(id)
    }

    fn dispose_context(&mut self, id: &ContextId) -> Result<(), CdpFailure> {
        self.ensure_open()?;
        if *id == self.default_context() {
            return Err(CdpFailure::invalid_argument("default context cannot be disposed"));
        }
        if self.contexts.remove(id).is_none() {
            return Err(CdpFailure::UnknownContext(*id));
        }
        let pages: Vec<PageId> = self
            .pages
            .values()
            .filter(|page| page.context_id == *id)
            .map(|page| page.id)
            .collect();
        for page in pages {
            let _ = self.remove_page_state(page);
        }
        Ok(())
    }

    fn create_page(&mut self, context: &ContextId, url: &str) -> Result<PageId, CdpFailure> {
        self.ensure_open()?;
        Self::validate_url(url)?;
        if !self.contexts.contains_key(context) {
            return Err(CdpFailure::UnknownContext(*context));
        }
        if self.pages.len() >= MAX_PAGES {
            return Err(CdpFailure::ActionQueueFull);
        }
        let id = PageId::new(Self::next_id(&mut self.next_page)?);
        self.pages.insert(
            id,
            PageState {
                id,
                context_id: *context,
                url: url.to_string(),
                title: String::new(),
                frame_id: format!("page-{id}"),
                loader_id: format!("loader-{id}-1"),
                document_generation: self.generation,
                network: PageNetworkState::default(),
            },
        );
        self.display.insert(id, PageDisplayState::default());
        Ok(id)
    }

    fn close_page(&mut self, page: &PageId) -> Result<(), CdpFailure> {
        self.ensure_open()?;
        self.remove_page_state(*page)
    }

    fn page_snapshot(&self, page: &PageId) -> Result<PageSnapshot, CdpFailure> {
        let Some(page) = self.pages.get(page) else {
            return Err(CdpFailure::UnknownPage(*page));
        };
        Ok(PageSnapshot {
            page_id: page.id,
            context_id: page.context_id,
            url: page.url.clone(),
            title: page.title.clone(),
            frame_id: page.frame_id.clone(),
            loader_id: page.loader_id.clone(),
            document_generation: page.document_generation,
        })
    }

    fn start_action(&mut self, action: EngineAction) -> Result<ActionId, CdpFailure> {
        self.ensure_open()?;
        if self.actions.len() >= MAX_ACTIONS {
            return Err(CdpFailure::ActionQueueFull);
        }
        let (page, context) = match &action {
            EngineAction::Navigate { page, .. }
            | EngineAction::FetchResource { page, .. }
            | EngineAction::Evaluate { page, .. }
            | EngineAction::CallFunctionOn { page, .. }
            | EngineAction::GetProperties { page, .. }
            | EngineAction::ReleaseObject { page, .. }
            | EngineAction::FetchScript { page, .. }
            | EngineAction::ResolveModule { page, .. }
            | EngineAction::DeliverInput { page, .. }
            | EngineAction::CaptureScreenshot { page, .. }
            | EngineAction::PrintToPdf { page, .. } => {
                if !self.pages.contains_key(page) {
                    return Err(CdpFailure::UnknownPage(*page));
                }
                (Some(*page), None)
            }
            EngineAction::LoadContext { context, .. } | EngineAction::StoreContext { context } => {
                if !self.contexts.contains_key(context) {
                    return Err(CdpFailure::UnknownContext(*context));
                }
                (None, Some(*context))
            }
            EngineAction::Wake { .. } => (None, None),
        };
        let id = ActionId::new(Self::next_id(&mut self.next_action)?);
        self.actions.insert(
            id,
            PendingAction {
                action,
                page,
                context,
                generation: self.generation,
            },
        );
        Ok(id)
    }

    fn complete_action(&mut self, id: ActionId, result: ActionResult) -> Result<(), CdpFailure> {
        let Some(pending) = self.actions.remove(&id) else {
            return Err(CdpFailure::UnknownAction(id));
        };
        if pending.generation != self.generation {
            return Err(CdpFailure::StaleAction(id));
        }
        if let ActionResult::Failed(failure) = result {
            return Err(failure);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_monotonic_and_context_disposal_invalidates_pages() {
        let mut state = BrowserState::new();
        let default = state.default_context();
        let context = state.create_context(ContextOptions::default()).unwrap();
        assert!(context > default);
        let first = state.create_page(&context, "about:blank").unwrap();
        state.close_page(&first).unwrap();
        let second = state.create_page(&context, "about:blank").unwrap();
        assert!(second > first);
        state.dispose_context(&context).unwrap();
        assert_eq!(state.page_snapshot(&second), Err(CdpFailure::UnknownPage(second)));
        assert_eq!(state.dispose_context(&default), Err(CdpFailure::InvalidArgument("default context cannot be disposed".to_string())));
    }

    #[test]
    fn sessions_and_actions_are_invalidated_on_close() {
        let mut state = BrowserState::new();
        let connection = state.open_connection().unwrap();
        let default = state.default_context();
        let page = state.create_page(&default, "about:blank").unwrap();
        let session = state.attach(connection, page).unwrap();
        assert_eq!(state.attached_page(session), Some(page));
        assert!(state.close_connection(connection));
        assert_eq!(state.attached_page(session), None);
        let connection = state.open_connection().unwrap();
        let session = state.attach(connection, page).unwrap();
        let action = state.start_action(EngineAction::Evaluate { page, expression: "1+1".into() }).unwrap();
        state.close_page(&page).unwrap();
        assert!(state.attached_page(session).is_none());
        assert_eq!(state.complete_action(action, ActionResult::unit()), Err(CdpFailure::UnknownAction(action)));
    }

    #[test]
    fn close_is_terminal_and_rejects_new_state() {
        let mut state = BrowserState::new();
        state.close();
        assert!(state.is_closed());
        assert_eq!(state.open_connection(), Err(CdpFailure::Closed));
        assert_eq!(state.create_context(ContextOptions::default()), Err(CdpFailure::Closed));
    }
}
