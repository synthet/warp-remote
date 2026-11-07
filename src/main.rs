use clap::Parser;
use remote_control_server::auth;
use remote_control_server::launch::{self, Args};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    let token = match args.configured_token()? {
        Some(token) => token,
        None => {
            let token = auth::generate_token();
            eprintln!("No SYNTH_RC_TOKEN configured; generated one for this run:\n\n    {token}\n");
            token
        }
    };

    let listener = launch::bind(&args).await?;
    launch::serve(listener, &args, token, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}
