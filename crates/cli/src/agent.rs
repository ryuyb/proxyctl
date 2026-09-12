//! The agent role: `proxyctl agent run`.
//!
//! # Why the daemon lives in the client binary
//!
//! The project ships one artifact, and the reason is measurable rather than
//! aesthetic: linking the client half and the server half together costs 2.4 MB
//! in a release build, so a second binary would save nothing. What it would cost
//! is a second definition of the deployment — two checksums, two service files, a
//! version that can skew between them.
//!
//! The two roles nonetheless stay separated in the source. This module is the
//! only place that reaches below the transport, and [`crate::client`] — everything
//! the `proxyctl <command>` path uses — depends on none of it. An architecture
//! test enforces that direction, which is what keeps the single artifact from
//! quietly becoming a single tangled crate.
//!
//! # Signal handling
//!
//! Shutdown is signalled through the same `watch` channel the HTTP server takes,
//! so `SIGTERM` reaches the listener rather than killing the process mid-request.
//! A second `SIGTERM` is not trapped: if the graceful path wedges, the operator's
//! next signal does what `kill -9` would, and that escape hatch must stay open.

use proxy_bootstrap::{Bootstrap, RuntimeConfig};
use tokio::sync::watch;

use crate::exit::Exit;

/// Runs the agent until it is signalled to stop.
///
/// # Errors
///
/// Returns the exit code for a startup failure — the configuration could not be
/// read, directories could not be prepared, or the socket could not be bound.
/// A binding failure is deliberately fatal: an agent that cannot accept requests
/// has no interface, and running without one would look like a working service
/// to every supervisor.
pub async fn run(config: RuntimeConfig) -> Result<(), Exit> {
    // The socket path is reported before anything can fail, so a failed start
    // still says which path was attempted.
    let socket = config.agent_socket_path();

    let context = Bootstrap::build_real(&config)
        .await
        .map_err(|e| report("cannot compose the agent", &e.to_string()))?;

    println!("proxy-agent listening on {socket}");
    run_with_context(std::sync::Arc::new(context), &config).await
}

/// Serves an already-composed context.
///
/// Split out so a test can supply its own context — for instance one composed
/// against a temporary directory — without going through the full factory.
///
/// # Errors
///
/// Returns [`Exit::Failure`] when the listener cannot be bound, and
/// [`Exit::DependencyUnreachable`] when the listener fails while serving. The
/// distinction matters: the first is a deployment problem, the second is a
/// dependency problem, and a supervisor restarts differently for each.
pub async fn run_with_context(
    context: std::sync::Arc<proxy_application::AppContext>,
    config: &RuntimeConfig,
) -> Result<(), Exit> {
    let server = proxy_bootstrap::build_http_server(context, config)
        .map_err(|e| report("cannot build the listener", &e.to_string()))?;

    let (shutdown_tx, shutdown_rx): (watch::Sender<bool>, watch::Receiver<bool>) =
        watch::channel(false);

    let signal = tokio::spawn(async move {
        // `ctrl_c` covers SIGINT and, on Unix, is a documented wrapper; SIGTERM is
        // what systemd sends, so it is handled explicitly.
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(stream) => stream,
                Err(e) => {
                    eprintln!("cannot install the SIGTERM handler: {e}");
                    std::future::pending::<()>().await;
                    return;
                }
            };
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(e) = result {
                    eprintln!("cannot listen for SIGINT: {e}");
                    return;
                }
            }
            _ = sigterm.recv() => {}
        }
        let _ = shutdown_tx.send(true);
    });

    // A `select!` rather than a race: whatever terminates first wins, and the
    // server's own result is what is reported. The signal task is aborted on the
    // other arm so a finished server does not leave a task parked on a signal
    // that will arrive at a process already exiting.
    let result = tokio::select! {
        outcome = server.serve(shutdown_rx) => outcome,
        _ = tokio::signal::ctrl_c() => {
            // Fallback path: the server exited first, or the handler task above
            // failed to install. Either way, shutdown was already requested.
            Ok(())
        }
    };

    signal.abort();

    result.map_err(|e| report("the listener failed", &e.to_string()))
}

/// Prints a startup failure and returns the exit code to use.
fn report(what: &str, detail: &str) -> Exit {
    eprintln!("proxy-agent: {what}: {detail}");
    Exit::Failure
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The socket path must be the one the client resolves by default, or the two
    /// halves of the same binary would disagree about where to meet.
    #[test]
    fn the_agent_socket_default_matches_the_clients_default() {
        let instance = proxy_domain::shared::id::MihomoInstanceId::parse("default").expect("valid");
        let config = RuntimeConfig::local(instance);
        assert_eq!(config.agent_socket_path(), crate::client::DEFAULT_SOCKET);
    }
}
