use crazyagent::protocol;

#[path = "../runner.rs"]
mod runner;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).take(4).collect();
    if args.len() != 2 || args[0] != "serve" {
        return Err("usage: crazyagent-runner serve PRIVATE_STATE_DIRECTORY".into());
    }
    assert_eq!(args.len(), 2);
    assert_eq!(args[0], "serve");
    #[cfg(unix)]
    rustix::process::setsid().map_err(|error| format!("runner_session_detach: {error}"))?;
    runner::run(std::path::Path::new(&args[1])).await?;
    Ok(())
}
