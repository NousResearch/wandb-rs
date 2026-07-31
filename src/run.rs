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

#[derive(Debug, Serialize)]
struct FsFinishData {
    complete: bool,
    exitcode: i32,
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

/// Tell the file stream that the run is over.
///
/// An exit code of 0 leaves the run in the `finished` state, anything else
/// marks it `failed`. Until this arrives the backend only knows the run by its
/// file stream traffic, so a run that never sends it is eventually flipped to
/// `crashed`.
async fn submit_finish(
    client: &reqwest::Client,
    run_path: &str,
    exit_code: i32,
) -> Result<(), ApiError> {
    let finish = FsFinishData {
        complete: true,
        exitcode: exit_code,
    };

    client
        .post(run_path)
        .json(&finish)
        .send()
        .await?
        .maybe_err()
        .await?;
    Ok(())
}

enum RunMessage {
    LogData { log_data: LogData, timestamp: f64 },
    FinishRun { exit_code: i32 },
}

impl Run {
    pub fn new(
        base_url: String,
        client: reqwest::Client,
        entity: String,
        project: String,
        name: String,
    ) -> Run {
        let (tx_log_data, mut rx_log_data) = mpsc::channel::<RunMessage>(10);
        let log_thread: JoinHandle<Result<(), ApiError>> = tokio::spawn(async move {
            let run_path = format!("{base_url}/files/{entity}/{project}/{name}/file_stream");
            let mut step = 0;
            let mut finish_result = Ok(());
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
                    RunMessage::FinishRun { exit_code } => {
                        if let Err(finish_error) =
                            submit_finish(&client, &run_path, exit_code).await
                        {
                            error!("Failed to mark WandB run {name} as finished: {finish_error}");
                            finish_result = Err(finish_error);
                        }
                        // finish() drops the sender right after queueing this,
                        // so there is nothing left to drain.
                        break;
                    }
                }
                step += 1;
            }
            info!("WandB run {name} ended.");
            finish_result
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

    /// Flush any pending log messages, mark the run as finished, and wait for
    /// the background upload task to stop.
    ///
    /// `log` only enqueues a row; the actual upload happens on a background
    /// task. If the program exits before that task drains the queue, the most
    /// recent rows are silently dropped. Call `finish().await` before exiting
    /// to make sure everything has been sent.
    ///
    /// This also tells the server the run is over. Without that record the
    /// backend keeps waiting for file stream traffic and eventually marks the
    /// run `crashed`, so a run that exits cleanly is indistinguishable from one
    /// that died.
    pub async fn finish(mut self) -> Result<(), ApiError> {
        // The channel is FIFO, so queueing the finish record here puts it
        // behind every row that has already been logged.
        if let Some(tx) = self.tx_log_data.take() {
            if let Err(send_error) = tx.send(RunMessage::FinishRun { exit_code: 0 }).await {
                warn!("Failed to send finish message to wandb: {send_error}");
            }
            // Dropping the sender closes the channel, so the background task's
            // recv() loop ends even if the finish record never made it in.
        }
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

#[cfg(test)]
mod test {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// Record every request body on one connection and answer each with the
    /// next status, until the client hangs up.
    fn serve(stream: TcpStream, statuses: Vec<u16>, bodies: Arc<Mutex<Vec<String>>>) {
        let mut writer = stream.try_clone().expect("clone stream");
        let mut reader = BufReader::new(stream);
        loop {
            let mut content_length = 0;
            loop {
                let mut line = String::new();
                if matches!(reader.read_line(&mut line), Ok(0) | Err(_)) {
                    return;
                }
                if line.trim_end().is_empty() {
                    break;
                }
                let line = line.to_ascii_lowercase();
                if let Some(value) = line.strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }

            let mut body = vec![0; content_length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
            // Record before answering so that a client which has seen its
            // response can rely on the body already being here. Counting from
            // the shared log rather than per connection keeps the statuses in
            // order even if the client opens a second connection.
            let served = {
                let mut recorded = bodies.lock().expect("lock bodies");
                recorded.push(String::from_utf8_lossy(&body).into_owned());
                recorded.len() - 1
            };
            let status = statuses
                .get(served)
                .or_else(|| statuses.last())
                .expect("at least one status");

            if writer
                .write_all(
                    format!("HTTP/1.1 {status} \r\ncontent-length: 2\r\n\r\n{{}}").as_bytes(),
                )
                .is_err()
            {
                return;
            }
        }
    }

    /// A file stream endpoint that records what gets posted to it and answers
    /// with `statuses` in order, repeating the last one once they run out.
    fn recording_endpoint(statuses: Vec<u16>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let base_url = format!("http://{}", listener.local_addr().expect("listener addr"));
        let bodies = Arc::new(Mutex::new(Vec::new()));

        let recorded = Arc::clone(&bodies);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let recorded = Arc::clone(&recorded);
                let statuses = statuses.clone();
                std::thread::spawn(move || serve(stream, statuses, recorded));
            }
        });

        (base_url, bodies)
    }

    fn test_run(base_url: String) -> Run {
        Run::new(
            base_url,
            reqwest::Client::new(),
            "entity".into(),
            "project".into(),
            "run".into(),
        )
    }

    #[tokio::test]
    async fn finish_sends_completion_record() {
        let (base_url, bodies) = recording_endpoint(vec![200]);

        let run = test_run(base_url);
        run.log((("loss", 0.5),)).await;
        run.finish().await.expect("finish run");

        let bodies = bodies.lock().expect("lock bodies");
        assert_eq!(bodies.len(), 2, "expected one log post and one finish post");

        let logged: serde_json::Value = serde_json::from_str(&bodies[0]).expect("log body");
        assert!(logged["files"]["wandb-history.jsonl"].is_object());
        assert!(logged["files"]["wandb-summary.json"].is_object());
        assert!(logged.get("complete").is_none());

        // The completion record has to come last, and carries no files.
        let finished: serde_json::Value = serde_json::from_str(&bodies[1]).expect("finish body");
        assert_eq!(finished["complete"], serde_json::json!(true));
        assert_eq!(finished["exitcode"], serde_json::json!(0));
        assert!(finished.get("files").is_none());
    }

    #[tokio::test]
    async fn finish_reports_a_rejected_completion_record() {
        // The row is accepted, the completion record is not.
        let (base_url, bodies) = recording_endpoint(vec![200, 500]);

        let run = test_run(base_url);
        run.log((("loss", 0.5),)).await;
        let result = run.finish().await;

        assert!(
            result.is_err(),
            "a rejected completion record should reach the caller"
        );
        assert_eq!(bodies.lock().expect("lock bodies").len(), 2);
    }
}
