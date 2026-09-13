//! Kernel control adapters.

pub mod adapter;
pub mod clash_relay;
pub mod connections;
pub mod framing;
pub mod http;
pub mod observer;
pub mod socket;
pub mod transport;
pub mod unix;
pub mod wire;

pub use adapter::HttpMihomoController;
pub use clash_relay::{ClashRelay, Endpoint as ControllerEndpoint};
pub use http::LoopbackTransport;
pub use socket::{SocketPermissions, default_socket_path};
pub use transport::{Method, Request, Response, Transport};
pub use unix::UnixSocketTransport;
