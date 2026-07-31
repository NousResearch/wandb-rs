use std::collections::HashMap;
use std::time::SystemTime;

use serde::Serialize;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{error, info, warn};

use crate::{data_value::LogData, ApiError, ReqwestBadResponse};

pub struct Run {
    tx_log_data: Option<mpsc::Sender<RunMessage>>,
    log_thread: Option<JoinHandle<Result<(), ApiError>>>,
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

const TIMESTAMP_METRIC_NAME: &str = "_timestamp";

async fn submit_log(
    client: &reqwest::Client,
    run_path: &str,
    step: u64,
    row: LogData,
) -> Result<(), ApiError> {
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

    client
        .post(run_path)
        .json(&log)
        .send()
        .await?
        .maybe_err()
        .await?;
    Ok(())
}

enum RunMessage {
    // TODO: add FinishRun
    LogData { log_data: LogData, timestamp: f64 },
}

impl Run {
    /// `start_offset` is the line the run's history file has already reached,
    /// i.e. the line this run should write next. It is 0 for a new run. See
    /// [`crate::WandB::new_run`], which reads it back from the server.
    pub fn new(
        base_url: String,
        client: reqwest::Client,
        entity: String,
        project: String,
        name: String,
        start_offset: u64,
    ) -> Run {
        let (tx_log_data, mut rx_log_data) = mpsc::channel::<RunMessage>(10);
        let log_thread: JoinHandle<Result<(), ApiError>> = tokio::spawn(async move {
            let run_path = format!("{base_url}/files/{entity}/{project}/{name}/file_stream");
            let mut step = start_offset;
            while let Some(message) = rx_log_data.recv().await {
                match message {
                    RunMessage::LogData {
                        mut log_data,
                        timestamp,
                    } => {
                        log_data.insert_default(TIMESTAMP_METRIC_NAME, timestamp);

                        if let Err(log_error) = submit_log(&client, &run_path, step, log_data).await
                        {
                            error!("Failed to log row to WandB for step {step}: {log_error}");
                        }
                    }
                }
                step += 1;
            }
            info!("WandB run {name} ended.");
            Ok(())
        });
        Run {
            tx_log_data: Some(tx_log_data),
            log_thread: Some(log_thread),
        }
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
    /// run.log((
    ///     ("epoch", 40),
    ///     ("train-loss", 0.5)
    /// ));
    /// ```
    /// See also [define_metric](https://docs.wandb.ai/ref/python/run#define_metric).
    pub async fn log(&self, row: impl Into<LogData>) {
        // hack to prevent nasty monomorphization blowup -
        // only the .into() is monomorphized, the rest is not.
        self._log(row.into()).await
    }
    async fn _log(&self, row: LogData) {
        let Some(tx) = self.tx_log_data.as_ref() else {
            warn!("log called after finish(); dropping row");
            return;
        };
        if let Err(e) = tx
            .send(RunMessage::LogData {
                log_data: row,
                timestamp: current_timestamp(),
            })
            .await
        {
            warn!("Failed to send log data to wandb: {}", e);
        }
    }

    /// Flush any pending log messages and wait for the background upload task
    /// to finish.
    ///
    /// `log` only enqueues a row; the actual upload happens on a background
    /// task. If the program exits before that task drains the queue, the most
    /// recent rows are silently dropped. Call `finish().await` before exiting
    /// to make sure everything has been sent.
    pub async fn finish(mut self) -> Result<(), ApiError> {
        // Dropping the sender closes the channel, so the background task's
        // recv() loop ends once the buffered messages have been drained.
        self.tx_log_data.take();
        match self.log_thread.take() {
            Some(handle) => match handle.await {
                Ok(result) => result,
                Err(join_error) => {
                    error!("wandb log task failed to join: {join_error}");
                    Ok(())
                }
            },
            None => Ok(()),
        }
    }
}

/// Get the current time in UNIX seconds
fn current_timestamp() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("System time was before the UNIX epoch")
        .as_secs_f64()
}
