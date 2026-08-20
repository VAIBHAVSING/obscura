//! Transport-free browser/context/target/session state for portable CDP.
//!
//! This module is intentionally independent of Tokio, sockets, V8 and the
//! native `Page` type.  It is the state layer that both the native dispatcher
//! and the future WASM dispatcher can use while a host remains responsible for
//! transport and asynchronous actions.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::engine::{
    ActionId, ActionResult, CdpEngine, CdpFailure, ContextId, ContextOptions, EngineAction,
    PageId, PageSnapshot,
};

pub const MAX_CONTEXTS: usize = 256;
pub const MAX_PAGES: usize = 4096;
pub const MAX_CONNECTIONS: usize = 512;
pub const MAX_SESSIONS: usize = 8192;
pub const MAX_ACTIONS: usize = 512;
pub const MAX_URL_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_STORAGE_STATE_BYTES: usize = 16 * 1024 * 1024;

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
                self.session_connections.remove(&session);
                self.sessions.remove(&session);
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
        self.sessions.remove(&id)
    }

    pub fn attached_page(&self, id: SessionId) -> Option<PageId> {
        self.sessions.get(&id).copied()
    }

    pub fn close(&mut self) {
        self.closed = true;
        self.contexts.clear();
        self.pages.clear();
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
            },
        );
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
