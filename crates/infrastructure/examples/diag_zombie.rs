//! Does an unreaped child remain visible in `/proc` after it exits?
//!
//! Determines whether `is_alive` can be fooled by a zombie the caller has not yet
//! waited on, which would make `stop` report a timeout for a dead process.
use std::time::Duration;

use tokio::process::Command;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all("/tmp/zbz/wd")?;
    std::fs::write(
        "/tmp/zbz/wd/config.yaml",
        "mixed-port: 17894\nbind-address: 127.0.0.1\nsecret: \"s\"\nmode: rule\n\
         log-level: warning\nproxies: []\nrules:\n  - MATCH,DIRECT\n",
    )?;

    // Spawned here and deliberately NOT waited on.
    let mut child = Command::new("/tmp/mhbin")
        .arg("-d")
        .arg("/tmp/zbz/wd")
        .arg("-f")
        .arg("/tmp/zbz/wd/config.yaml")
        .spawn()?;
    let pid = child.id().expect("pid");
    tokio::time::sleep(Duration::from_millis(800)).await;
    println!("before signal: stat exists = {}", stat_exists(pid));

    Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(contents) => {
            let state = contents.split_whitespace().nth(2).unwrap_or("?");
            println!("after signal: stat READABLE, state = [{state}]  (Z means zombie)");
            println!(">>> is_alive() would report TRUE for a zombie, so stop() would time out");
        }
        Err(e) => println!("after signal: stat gone ({e}) -> is_alive is correct"),
    }

    let _ = child.wait().await;
    println!("after wait(): stat exists = {}", stat_exists(pid));

    let _ = std::fs::remove_dir_all("/tmp/zbz");
    Ok(())
}

fn stat_exists(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}/stat")).exists()
}
