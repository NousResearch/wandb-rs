use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use reqwest::StatusCode;
use serde::Serialize;
use tokio::{sync::mpsc, task::JoinHandle};
use tracing::{error, info, warn};

use crate::{data_value::LogData, ApiError, ReqwestBadResponse};

/// How many rows may wait in the queue before `log` starts making the caller
/// wait for the uploader.
const LOG_QUEUE_CAPACITY: usize = 1024;

/// The most rows to put in one file stream request.
const MAX_BATCH_ROWS: usize = 128;

/// Attempts per batch, counting the first.
const MAX_UPLOAD_ATTEMPTS: u32 = 5;
const FIRST_RETRY_DELAY: Duration = Duration::from_millis(200);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);

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

/// Upload a run of consecutive rows, the first of which belongs at line
/// `step` of the history file.
async fn submit_log(
    client: &reqwest::Client,
    run_path: &str,
    step: u64,
    rows: &[LogData],
) -> Result<(), ApiError> {
    let mut history = Vec::with_capacity(rows.len());
    for row in rows {
        history.push(serde_json::to_string(row)?);
    }
    // The summary is one line the backend keeps overwriting with the newest
    // values, so only the last row of the batch is worth sending.
    let Some(summary) = history.last().cloned() else {
        return Ok(());
    };

    let log = FsFilesData {
        files: [
            (
                "wandb-history.jsonl".to_string(),
                FsChunkData {
                    content: history,
                    offset: step,
                },
            ),
            (
                "wandb-summary.json".to_string(),
                FsChunkData {
                    content: vec![summary],
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

/// Whether a failed upload is worth another attempt.
fn is_transient(error: &ApiError) -> bool {
    let error = match error {
        ApiError::RequestErrorWithBody(error) => &error.error,
        ApiError::RequestFailed(error) => error,
        _ => return false,
    };
    if error.is_timeout() || error.is_connect() {
        return true;
    }
    error
        .status()
        .is_some_and(|status| status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error())
}

/// Upload a batch, retrying transient failures with exponential backoff.
///
/// Retrying is safe: the file stream addresses lines by absolute offset, so a
/// resent batch rewrites the same lines rather than appending them twice.
async fn submit_log_with_retries(
    client: &reqwest::Client,
    run_path: &str,
    step: u64,
    rows: &[LogData],
) -> Result<(), ApiError> {
    let mut delay = FIRST_RETRY_DELAY;
    let mut attempt = 1;
    loop {
        let error = match submit_log(client, run_path, step, rows).await {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        if attempt >= MAX_UPLOAD_ATTEMPTS || !is_transient(&error) {
            return Err(error);
        }
        warn!(
            "Upload of {} row(s) at offset {step} failed on attempt {attempt} of \
             {MAX_UPLOAD_ATTEMPTS}, retrying in {delay:?}: {error}",
            rows.len()
        );
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_RETRY_DELAY);
        attempt += 1;
    }
}

enum RunMessage {
    // TODO: add FinishRun
    LogData { log_data: LogData, timestamp: f64 },
}

impl Run {
    pub fn new(
        base_url: String,
        client: reqwest::Client,
        entity: String,
        project: String,
        name: String,
    ) -> Run {
        let (tx_log_data, mut rx_log_data) = mpsc::channel::<RunMessage>(LOG_QUEUE_CAPACITY);
        let log_thread: JoinHandle<Result<(), ApiError>> = tokio::spawn(async move {
            let run_path = format!("{base_url}/files/{entity}/{project}/{name}/file_stream");
            let mut step = 0;
            let mut messages = Vec::with_capacity(MAX_BATCH_ROWS);
            let mut rows = Vec::with_capacity(MAX_BATCH_ROWS);
            // recv_many waits for one message, then takes whatever else is
            // already queued. A caller logging slowly still gets a request per
            // row; one logging faster than the network gets its rows coalesced,
            // without a timer adding latency to either.
            while rx_log_data.recv_many(&mut messages, MAX_BATCH_ROWS).await > 0 {
                rows.clear();
                for message in messages.drain(..) {
                    match message {
                        RunMessage::LogData {
                            mut log_data,
                            timestamp,
                        } => {
                            log_data.insert_default(TIMESTAMP_METRIC_NAME, timestamp);
                            rows.push(log_data);
                        }
                    }
                }

                if let Err(log_error) =
                    submit_log_with_retries(&client, &run_path, step, &rows).await
                {
                    error!(
                        "Failed to log {} row(s) to WandB at offset {step}: {log_error}",
                        rows.len()
                    );
                }
                // Advance either way. A batch that could not be delivered
                // leaves its lines empty, rather than shifting every later row
                // onto the wrong line.
                step += rows.len() as u64;
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

#[cfg(test)]
mod test {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// Every request body the mock endpoint was sent.
    type Bodies = Arc<Mutex<Vec<String>>>;

    /// Record each request body and answer with the next scripted status,
    /// until the client hangs up.
    fn serve(stream: TcpStream, statuses: Vec<u16>, hold_first: Duration, bodies: Bodies) {
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
            // Record before answering, and count from the shared log so the
            // statuses stay in order even across connections.
            let served = {
                let mut recorded = bodies.lock().expect("lock bodies");
                recorded.push(String::from_utf8_lossy(&body).into_owned());
                recorded.len() - 1
            };
            if served == 0 {
                // Keep the first upload in flight so rows queue behind it.
                std::thread::sleep(hold_first);
            }
            let status = statuses
                .get(served)
                .or_else(|| statuses.last())
                .expect("at least one status");

            let response = format!("HTTP/1.1 {status} \r\ncontent-length: 2\r\n\r\n{{}}");
            if writer.write_all(response.as_bytes()).is_err() {
                return;
            }
        }
    }

    fn recording_endpoint(statuses: Vec<u16>, hold_first: Duration) -> (String, Bodies) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let base_url = format!("http://{}", listener.local_addr().expect("listener addr"));
        let bodies: Bodies = Arc::new(Mutex::new(Vec::new()));

        let recorded = Arc::clone(&bodies);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let recorded = Arc::clone(&recorded);
                let statuses = statuses.clone();
                std::thread::spawn(move || serve(stream, statuses, hold_first, recorded));
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

    /// The (offset, row count) of a request's history chunk.
    fn history_chunk(body: &str) -> (u64, usize) {
        let body: serde_json::Value = serde_json::from_str(body).expect("request body");
        let chunk = &body["files"]["wandb-history.jsonl"];
        (
            chunk["offset"].as_u64().expect("offset"),
            chunk["content"].as_array().expect("content").len(),
        )
    }

    #[tokio::test]
    async fn rows_queued_during_an_upload_go_out_together() {
        let (base_url, bodies) = recording_endpoint(vec![200], Duration::from_millis(300));

        let run = test_run(base_url);
        run.log((("i", 0u64),)).await;
        // Let the uploader pick up the first row and block on it.
        tokio::time::sleep(Duration::from_millis(50)).await;
        for i in 1..5u64 {
            run.log((("i", i),)).await;
        }
        run.finish().await.expect("finish run");

        let bodies = bodies.lock().expect("lock bodies");
        let chunks: Vec<(u64, usize)> = bodies.iter().map(|body| history_chunk(body)).collect();

        assert_eq!(chunks.len(), 2, "the queued rows should have coalesced");
        assert_eq!(chunks[0], (0, 1));
        assert_eq!(
            chunks[1],
            (1, 4),
            "the batch resumes where the first stopped"
        );

        // Whatever the batching, every row lands exactly once and the offsets
        // stay contiguous.
        let mut next = 0;
        for (offset, count) in &chunks {
            assert_eq!(*offset, next);
            next += *count as u64;
        }
        assert_eq!(next, 5);
    }

    #[tokio::test]
    async fn a_transient_failure_is_retried() {
        let (base_url, bodies) = recording_endpoint(vec![503, 200], Duration::ZERO);

        let run = test_run(base_url);
        run.log((("loss", 0.5),)).await;
        run.finish().await.expect("finish run");

        let bodies = bodies.lock().expect("lock bodies");
        assert_eq!(bodies.len(), 2, "the 503 should have been retried");
        // The retry rewrites the same line rather than appending a second one.
        assert_eq!(history_chunk(&bodies[0]), (0, 1));
        assert_eq!(history_chunk(&bodies[1]), (0, 1));

        // Compared as JSON, since `files` is a HashMap and its two keys come
        // out in either order.
        let first: serde_json::Value = serde_json::from_str(&bodies[0]).expect("first body");
        let second: serde_json::Value = serde_json::from_str(&bodies[1]).expect("second body");
        assert_eq!(first, second, "the retry should resend the same batch");
    }

    #[tokio::test]
    async fn a_client_error_is_not_retried() {
        let (base_url, bodies) = recording_endpoint(vec![400], Duration::ZERO);

        let run = test_run(base_url);
        run.log((("loss", 0.5),)).await;
        run.finish().await.expect("finish run");

        assert_eq!(
            bodies.lock().expect("lock bodies").len(),
            1,
            "a 400 will not succeed on a second attempt"
        );
    }
}
