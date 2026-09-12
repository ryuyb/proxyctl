//! `proxyctl token` — managing credentials for the network listener.
//!
//! # Why these do not go through the socket
//!
//! Every other command talks to a running agent over the unix socket. These do
//! not, and the reason is ordering: a token is what makes the TCP listener
//! *allowed to exist*, so issuing one has to work before the agent is listening —
//! including on a fresh install where it has never run.
//!
//! That makes these the only commands that open the metadata database directly.
//! The alternative would be bootstrap ordering: start the agent with no listener,
//! issue a token over the socket, restart it with one. That is three steps and a
//! window where the deployment is half-configured, to avoid a direct write the
//! command is already trusted to perform.
//!
//! The database work itself lives in `proxy-bootstrap`, because this crate is not
//! allowed to depend on the storage adapters — see that module for the reasoning.
//!
//! # The token is printed once
//!
//! Only a hash is stored, so there is no way to print it again. A caller that
//! loses it must issue a new one, which is why the output says so rather than
//! leaving someone to discover it from an authentication failure.

use proxy_application::ports::secret_store::Role;
use proxy_bootstrap::RuntimeConfig;
use proxy_bootstrap::tokens::SqliteSecretStore;

use crate::args::{TokenIssueArgs, TokenRevokeArgs};
use crate::exit::Exit;

/// Issues a token and prints it.
pub async fn issue(args: &TokenIssueArgs) -> Exit {
    let store = match open(&args.config).await {
        Ok(store) => store,
        Err(e) => {
            eprintln!("proxyctl: {e}");
            return Exit::Failure;
        }
    };

    let role: Role = args.role.into();
    match store.issue(&args.principal, role).await {
        Ok(token) => {
            // The role is echoed because it is the part a caller most easily gets
            // wrong, and the error it causes is a refusal at request time rather
            // than anything visible here.
            println!("principal: {}", args.principal);
            println!("role:      {}", describe(role));
            println!("token:     {token}");
            println!();
            println!(
                "Store this now: only a hash is kept, so it cannot be shown again. \
                 Issuing another\nreplaces this one."
            );
            Exit::Success
        }
        Err(e) => {
            eprintln!("proxyctl: cannot issue a token: {e}");
            Exit::Failure
        }
    }
}

/// Lists principals.
pub async fn list(config: &Option<std::path::PathBuf>) -> Exit {
    let store = match open(config).await {
        Ok(store) => store,
        Err(e) => {
            eprintln!("proxyctl: {e}");
            return Exit::Failure;
        }
    };

    match store.list().await {
        Ok(principals) if principals.is_empty() => {
            // A caller checking whether a listener can start reads this, so the
            // message says what the empty state *means* rather than only that it is
            // empty.
            println!("no API tokens are issued");
            println!();
            println!(
                "The network listener cannot start without one. Issue one with \
                 `proxyctl token issue --principal <name>`, or leave [api] bind unset to keep the \
                 agent on the unix socket only."
            );
            Exit::Success
        }
        Ok(principals) => {
            for principal in &principals {
                println!(
                    "{:<24} {:<10} {}",
                    principal.id,
                    describe(principal.role),
                    principal.created_at
                );
            }
            println!("{} principal(s)", principals.len());
            Exit::Success
        }
        Err(e) => {
            eprintln!("proxyctl: cannot list tokens: {e}");
            Exit::Failure
        }
    }
}

/// Revokes a token.
pub async fn revoke(args: &TokenRevokeArgs) -> Exit {
    let store = match open(&args.config).await {
        Ok(store) => store,
        Err(e) => {
            eprintln!("proxyctl: {e}");
            return Exit::Failure;
        }
    };

    match store.revoke(&args.principal).await {
        Ok(true) => {
            println!("revoked the token for {}", args.principal);
            Exit::Success
        }
        // Not a failure: the caller's intent — this principal must not
        // authenticate — holds either way. Reporting an error would make a script
        // that revokes defensively fail on its second run.
        Ok(false) => {
            println!("{} has no token to revoke", args.principal);
            Exit::Success
        }
        Err(e) => {
            eprintln!("proxyctl: cannot revoke the token: {e}");
            Exit::Failure
        }
    }
}

/// Opens the token store named by the configuration.
///
/// The configuration goes through the same merge the daemon uses, so these
/// commands cannot disagree with it about where the database lives — a
/// disagreement that would have them issue tokens into a file the agent never
/// opens.
async fn open(config: &Option<std::path::PathBuf>) -> Result<SqliteSecretStore, String> {
    let path = config
        .clone()
        .unwrap_or_else(|| proxy_bootstrap::config::file::resolve_path(None));
    let file = proxy_bootstrap::config::file::load(&path).map_err(|e| e.to_string())?;

    // Only the paths matter here, so the merge is used for its resolution rather
    // than for its full configuration; nothing else is composed, which is what lets
    // this work on a machine where the agent has never run.
    let inputs = proxy_bootstrap::config::merge::Inputs {
        file,
        ..Default::default()
    };
    let resolved = proxy_bootstrap::config::merge::merge(&inputs).map_err(|e| e.to_string())?;

    // No runtime is created here. These commands are already driven from the
    // binary's runtime, and `block_on` inside one panics — which is exactly what an
    // earlier version of this function did.
    let config = RuntimeConfig {
        paths: resolved.config.paths,
        ..resolved.config
    };
    proxy_bootstrap::tokens::open_store(&config)
        .await
        .map_err(|e| e.to_string())
}

/// A role's label.
fn describe(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::ReadOnly => "read-only",
    }
}
