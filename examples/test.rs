use std::{env, time::Duration};

use wandb::{BackendOptions, RunInfo, WandB};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let api_key = env::var("API_KEY").map_err(|_| "no API_KEY env var.")?;
    let wandb = WandB::new(BackendOptions::new(api_key));

    let run = wandb
        .new_run(
            RunInfo::new("wandb-rs")
                .entity("nous_research")
                .name(format!(
                    "test-{}",
                    std::time::UNIX_EPOCH.elapsed().unwrap().as_millis()
                ))
                .config((
                    ("bees", 150),
                    ("go crazy?", true),
                    ("favorite fish", "white tuna"),
                ))
                .build()?,
        )
        .await?;
    for i in 0..100 {
        run.log((
            ("_step", i),
            ("loss", 1.0 / (i as f64).sqrt()),
            ("hellaswag", i as f64 % 100.0),
        ))
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}
