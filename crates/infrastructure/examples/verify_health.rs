//! Classifies the health report measured from a real kernel whose proxy listener
//! failed to bind while its control API stayed reachable.
//!
//! Run against a live kernel with `PROXYCTL_TEST_CONTROLLER` set; without it the
//! example uses the measured snapshot so it is still illustrative.

use std::sync::Arc;
use std::time::Duration;

use proxy_application::ports::mihomo_controller::MihomoController;
use proxy_application::ports::types::HealthReport;
use proxy_infrastructure::mihomo::{HttpMihomoController, LoopbackTransport};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let report = match (
        std::env::var("PROXYCTL_TEST_CONTROLLER").ok(),
        std::env::var("PROXYCTL_TEST_SECRET").ok(),
    ) {
        (Some(address), Some(secret)) => {
            let transport = LoopbackTransport::new(&address, &secret, Duration::from_secs(5))?;
            HttpMihomoController::new(Arc::new(transport))
                .health_check()
                .await?
        }
        _ => {
            // Measured on a live kernel with an occupied mixed port: the API
            // answered while the listener never bound.
            println!("(using the measured snapshot; set PROXYCTL_TEST_CONTROLLER to probe live)");
            HealthReport {
                process_alive: true,
                controller_reachable: true,
                config_loaded: true,
                proxy_port_listening: false,
            }
        }
    };

    println!("report    = {}", report.summary());
    println!("healthy   = {}", report.is_healthy());
    println!("degraded  = {}", report.is_degraded());
    println!("unhealthy = {}", report.is_unhealthy());

    if !report.proxy_port_listening && report.controller_reachable {
        println!(
            "NOTE: the control API answered but no inbound port is listening. \
             An API-only check would have reported this instance as healthy."
        );
    }

    Ok(())
}
