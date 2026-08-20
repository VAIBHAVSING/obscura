pub mod action;
pub mod engine;
pub mod protocol;
pub mod state;

// `server` is the legacy native TCP/WebSocket transport. Keep it out of the
// portable graph: it owns listeners, threads, signals and Tokio networking.
#[cfg(feature = "native-server")]
pub mod server;

// The current dispatcher/domain implementation owns native BrowserContext,
// Page and V8 state. It remains available to the native adapter while the
// transport-free protocol/state core is migrated into the portable feature.
#[cfg(feature = "native-engine")]
pub mod dispatch;
pub mod types;
#[cfg(feature = "native-engine")]
pub mod domains;
#[cfg(feature = "native-engine")]
pub mod cookie_params;
#[cfg(feature = "native-engine")]
pub(crate) mod util;

#[cfg(feature = "native-server")]
pub use server::{
    start, start_with_full_options, start_with_full_serve_options, start_with_host,
    start_with_host_and_security, start_with_options, start_with_serve_options_and_limit,
    DEFAULT_MAX_CONNECTIONS,
};
