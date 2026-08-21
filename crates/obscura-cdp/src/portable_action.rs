//! Data-only mapping from portable CDP host work to [`EngineAction`].
//!
//! The host still performs JavaScript, network, input and rendering work, but
//! it must not decide which protocol operation a queued action represents.
//! Keeping this mapping beside the portable engine contract prevents the
//! WASM adapter from becoming a second protocol implementation.

use serde_json::Value;

use crate::engine::{CdpFailure, EngineAction, PageId};

/// Build the target-neutral action requested by a portable CDP command.
pub fn from_kind(page: PageId, kind: &str, payload: &Value) -> Result<EngineAction, CdpFailure> {
    let string_field = |name: &str, default: &str| {
        payload
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_string()
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
        "getIsolateId" => EngineAction::Wake { deadline_millis: 0 },
        _ => {
            return Err(CdpFailure::Unsupported(format!(
                "unknown host action kind {kind}"
            )))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_runtime_navigation_input_and_capture_actions() {
        let page = PageId::new(7);
        assert!(matches!(
            from_kind(page, "navigate", &serde_json::json!({"url": "https://example.test"})),
            Ok(EngineAction::Navigate { page: actual, .. }) if actual == page
        ));
        assert!(matches!(
            from_kind(page, "evaluate", &serde_json::json!({"expression": "1 + 1"})),
            Ok(EngineAction::Evaluate { page: actual, expression }) if actual == page && expression == "1 + 1"
        ));
        assert!(matches!(
            from_kind(page, "dispatchMouseEvent", &serde_json::json!({"type": "mousePressed"})),
            Ok(EngineAction::DeliverInput { page: actual, .. }) if actual == page
        ));
        assert!(matches!(
            from_kind(page, "pdf", &serde_json::json!({"landscape": true})),
            Ok(EngineAction::PrintToPdf { page: actual, .. }) if actual == page
        ));
    }

    #[test]
    fn rejects_unknown_action_kinds_without_host_side_effects() {
        let error = from_kind(PageId::new(1), "domGetDocument", &Value::Null).unwrap_err();
        assert!(
            matches!(error, CdpFailure::Unsupported(message) if message.contains("domGetDocument"))
        );
    }
}
