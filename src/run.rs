use std::collections::HashMap;

use serde::Serialize;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::warn;

use crate::{data_value::LogData, ApiError};

pub struct Run {
    tx_log_data: mpsc::Sender<LogData>,
}

#[derive(Debug, Serialize)]
struct FsChunkData {
    content: Vec<String>,
    offset: u64,
}

#[derive(Debug, Serialize)]
struct FsFilesData {
    files: HashMap<String, FsChunkData>,
}

impl Run {
    pub fn new(
        base_url: String,
        client: reqwest::Client,
        entity: String,
        project: String,
        name: String,
    ) -> Run {
        let (tx_log_data, mut rx_log_data) = mpsc::channel::<LogData>(10);
        let _: JoinHandle<Result<(), ApiError>> = tokio::spawn(async move {
            let run_path = format!("{base_url}/files/{entity}/{project}/{name}/file_stream");
            let mut step = 0;
            loop {
                if let Some(row) = rx_log_data.recv().await {
                    let row_string = serde_json::to_string(&row)?;
                    let log = FsFilesData {
                        files: [
                            (
                                "wandb-history.jsonl".to_string(),
                                FsChunkData {
                                    content: vec![row_string.clone()],
                                    offset: step,
                                },
                            ),
                            (
                                "wandb-summary.json".to_string(),
                                FsChunkData {
                                    content: vec![row_string.clone()],
                                    offset: 0,
                                },
                            ),
                        ]
                        .into_iter()
                        .collect(),
                    };

                    let x = client
                        .post(&run_path)
                        .json(&log)
                        .send()
                        .await?
                        .text()
                        .await?;
                    dbg!(x);
                    step += 1;
                } else {
                    break;
                }
            }
            Ok(())
        });
        Run { tx_log_data }
    }

    /// Upload run data.

    /// Use `log` to log data from runs, such as scalars, images, vereo,
    /// histograms, plots, and tables.
    ///
    /// See our [guides to logging](https://docs.wandb.ai/guides/track/log) for
    /// live examples, code snippets, best practices, and more.
    ///
    /// The most basic usage is `run.log(("train-loss", 0.5), ("accuracy", 0.9))`.
    /// This will save the loss and accuracy to the run's history and update
    /// the summary values for these metrics.
    ///
    /// Visualize logged data in the workspace at [wandb.ai](https://wandb.ai),
    /// or locally on a [self-hosted instance](https://docs.wandb.ai/guides/hosting)
    /// of the W&B app, or export data to visualize and explore locally, e.g. in
    /// Jupyter notebooks, with [our API](https://docs.wandb.ai/guides/track/public-api-guide).
    ///
    /// Logged values don't have to be scalars. Logging any wandb object is supported.
    /// For example `run.log({"example": wandb.Image("myimage.jpg")})` will log an
    /// example image which will be displayed nicely in the W&B UI.
    /// See the [reference documentation](https://docs.wandb.com/ref/python/data-types)
    /// for all of the different supported types or check out our
    /// [guides to logging](https://docs.wandb.ai/guides/track/log) for examples,
    /// from 3D molecular structures and segmentation masks to PR curves and histograms.
    /// You can use `wandb.Table` to log structured data. See our
    /// [guide to logging tables](https://docs.wandb.ai/guides/data-vis/log-tables)
    /// for details.
    ///
    /// The W&B UI organizes metrics with a forward slash (`/`) in their name
    /// into sections named using the text before the final slash. For example,
    /// the following results in two sections named "train" and "validate":
    ///
    /// ```
    /// run.log((
    ///     ("train/accuracy", 0.9),
    ///     ("train/loss", 30),
    ///     ("validate/accuracy", 0.8),
    ///     ("validate/loss", 20),
    /// ));
    /// ```
    ///
    /// Only one level of nesting is supported; `run.log({"a/b/c": 1})`
    /// produces a section named "a/b".
    ///
    /// `run.log` is not intended to be called more than a few times per second.
    /// For optimal performance, limit your logging to once every N iterations,
    /// or collect data over multiple iterations and log it in a single step.
    ///
    /// ### The W&B step
    ///
    /// With basic usage, each call to `log` creates a new "step".
    /// The step must always increase, and it is not possible to log
    /// to a previous step.
    ///
    /// Note that you can use any metric as the X axis in charts.
    /// In many cases, it is better to treat the W&B step like
    /// you'd treat a timestamp rather than a training step.
    ///
    /// ```
    /// // Example: log an "epoch" metric for use as an X axis.
    /// run.log((("epoch", 40), ("train-loss", 0.5)));
    /// ```

    /// See also [define_metric](https://docs.wandb.ai/ref/python/run#define_metric).

    pub async fn log(&self, row: impl Into<LogData>) {
        // hack to prevent nasty monomorphization blowup -
        // only the .into() is monomorphized, the rest is not.
        self._log(row.into()).await
    }
    async fn _log(&self, row: LogData) {
        if let Err(e) = self.tx_log_data.send(row).await {
            warn!("Failed to send log data to wandb: {}", e);
        }
    }
}
