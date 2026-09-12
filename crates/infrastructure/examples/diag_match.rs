//! Diagnoses why discovery does or does not match a process.
//!
//! Used during development to compare the matcher against real `/proc` contents.
use proxy_application::ports::process_manager::StartOptions;
use proxy_infrastructure::process::procinfo;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let pid: u32 = args.get(1).expect("pid argument").parse()?;
    let binary = args.get(2).expect("binary argument").clone();
    let dir = args.get(3).expect("dir argument").clone();

    println!("--- /proc facts for pid {pid} ---");
    println!("exe     : {:?}", procinfo::exe_of(pid).await?);
    println!("cmdline : {:?}", procinfo::cmdline_of(pid).await?);
    println!("cwd     : {:?}", procinfo::cwd_of(pid).await?);
    println!("start   : {:?}", procinfo::start_time_of(pid).await?);

    let options = StartOptions {
        binary_path: binary.clone(),
        working_dir: dir.clone(),
        config_path: format!("{dir}/config.yaml"),
        required_capabilities: Vec::new(),
    };
    println!("--- matching against ---");
    println!("binary_path: {binary}");
    println!("working_dir: {dir}");
    println!(
        "matches_kernel = {}",
        procinfo::matches_kernel(pid, &options).await?
    );

    Ok(())
}
