//! Shared transport-free CDP domain routing.
//!
//! The portable adapter owns host concerns such as JavaScript execution,
//! rendering and event queues.  Domain selection and all state-only protocol
//! commands belong here so a WASM host does not grow a second CDP dispatcher.
//! A `None` result means that the command still requires a host-backed domain
//! and must be handled by the adapter's action/runtime path.

use crate::engine::PageId;
use crate::protocol::{CdpRequest, CdpResponse};
use crate::state::{BrowserState, SessionId};

/// Whether the shared Browser/Target dispatcher owns this command.
pub fn supports_browser(method: &str) -> bool {
    crate::portable_target::supports(method)
}

/// Dispatch a state-only Browser/Target command.
pub fn dispatch_browser(request: &CdpRequest, state: &mut BrowserState) -> Option<CdpResponse> {
    supports_browser(&request.method).then(|| crate::portable_target::dispatch(request, state))
}

/// Whether the shared page-domain dispatcher owns this command.
pub fn supports_page(method: &str) -> bool {
    crate::portable_storage::supports(method)
        || crate::portable_fetch::supports(method)
        || crate::portable_emulation::supports(method)
        || crate::portable_network::supports(method)
        || crate::portable_page::supports(method)
}

/// Dispatch one state-only page command through the shared domain modules.
///
/// Domain ordering is intentional: storage and Fetch overlap with Network
/// method names, and the more specific handlers must win before the generic
/// Network policy handler sees a command.
pub fn dispatch_page(
    request: &CdpRequest,
    state: &mut BrowserState,
    page_id: PageId,
    session_id: Option<SessionId>,
) -> Option<CdpResponse> {
    if crate::portable_storage::supports(&request.method) {
        return Some(crate::portable_storage::dispatch(request, state, page_id));
    }
    if crate::portable_fetch::supports(&request.method) {
        return Some(crate::portable_fetch::dispatch(
            request, state, page_id, session_id,
        ));
    }
    if crate::portable_emulation::supports(&request.method) {
        return Some(crate::portable_emulation::dispatch(request, state, page_id));
    }
    if crate::portable_network::supports(&request.method) {
        return Some(crate::portable_network::dispatch(
            request, state, page_id, session_id,
        ));
    }
    if crate::portable_page::supports(&request.method) {
        return Some(crate::portable_page::dispatch(request, state, page_id));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CdpEngine;
    use serde_json::json;

    fn request(id: u64, method: &str, params: serde_json::Value) -> CdpRequest {
        CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some("page-1-session-1".to_string()),
        }
    }

    #[test]
    fn routes_browser_and_page_domains_without_transport_state() {
        let mut state = BrowserState::new();
        let version = dispatch_browser(&request(1, "Browser.getVersion", json!({})), &mut state)
            .expect("Browser command should be shared");
        assert_eq!(version.result.unwrap()["product"], "Obscura/WASM");

        let page = state
            .create_page(&state.default_context(), "about:blank")
            .unwrap();
        let metrics = dispatch_page(
            &request(2, "Page.getLayoutMetrics", json!({})),
            &mut state,
            page,
            None,
        )
        .expect("Page command should be shared");
        assert_eq!(
            metrics.result.unwrap()["layoutViewport"]["clientWidth"],
            800
        );
        assert!(!supports_page("Runtime.evaluate"));
    }

    #[test]
    fn unsupported_host_commands_are_left_for_the_adapter() {
        let mut state = BrowserState::new();
        let request = request(1, "Runtime.evaluate", json!({"expression": "1 + 1"}));
        assert!(dispatch_browser(&request, &mut state).is_none());
        assert!(dispatch_page(&request, &mut state, PageId::new(1), None).is_none());
    }
}
