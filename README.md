# Weights & Biases for Rust

Simple run creation & logging implemented!

```rs
let wandb = WandB::new(BackendOptions::new(api_key));

let run = wandb
    .new_run(
        RunInfo::new("wandb-rs")
            .entity("nous_research")
            .name("node-25")
            .build()?,
    )
    .await?;

for i in 0..100 {
    run.log((("_step", i), ("loss", 1.0 / (i as f64).sqrt())))
        .await;
}
```

see `examples/test.rs` for an example :)
