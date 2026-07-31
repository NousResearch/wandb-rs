use std::{fmt::Display, future::Future};

use base64::{prelude::BASE64_STANDARD as base64, Engine};
pub use data_value::{DataValue, LogData};
use gql::{upsert_bucket, UpsertBucket};
use graphql_client::GraphQLQuery;
pub use run::Run;

mod data_value;
mod gql;
mod run;

pub struct WandB {
    client: reqwest::Client,
    base_url: String,
}

#[derive(Default)]
pub struct RunInfo {
    project: String,
    entity: Option<String>,
    name: Option<String>,
    config: Option<LogData>,
    commit: Option<String>,
    group: Option<String>,
    host: Option<String>,
}

impl RunInfo {
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            ..Default::default()
        }
    }

    pub fn entity(mut self, entity: impl Into<String>) -> Self {
        self.entity = Some(entity.into());
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
    pub fn commit(mut self, commit: impl Into<String>) -> Self {
        self.commit = Some(commit.into());
        self
    }

    pub fn group(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    pub fn config(mut self, config: impl Into<LogData>) -> Self {
        self.config = Some(config.into());
        self
    }

    pub fn build(self) -> Result<upsert_bucket::Variables, serde_json::Error> {
        let config = self.config.map(|c| serde_json::to_string(&c)).transpose()?;
        Ok(upsert_bucket::Variables {
            entity: self.entity,
            name: self.name,
            commit: self.commit,
            config,
            project: self.project.into(),
            id: None,
            debug: None,
            description: None,
            display_name: None,
            group_name: self.group,
            host: self.host,
            job_type: None,
            notes: None,
            program: None,
            repo: None,
            state: None,
            summary_metrics: None,
            sweep: None,
            tags: None,
        })
    }
}

/// A custom error type that combines a Reqwest error with the response body.
///
/// This struct wraps a [`reqwest::Error`] and includes the response body as a string,
/// which can be useful for debugging and error reporting when HTTP requests fail.
#[derive(Debug)]
pub struct ReqwestErrorWithBody {
    error: reqwest::Error,
    body: Result<String, reqwest::Error>,
}

impl Display for ReqwestErrorWithBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Request error:",)?;
        writeln!(f, "{}", self.error)?;
        match &self.body {
            Ok(body) => {
                writeln!(f, "Response body:")?;
                writeln!(f, "{body}")?;
            }
            Err(err) => {
                writeln!(f, "Failed to fetch body:")?;
                writeln!(f, "{err}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for ReqwestErrorWithBody {}

pub trait ReqwestBadResponse {
    fn maybe_err(self) -> impl Future<Output = Result<Self, ReqwestErrorWithBody>>
    where
        Self: Sized;
}

impl ReqwestBadResponse for reqwest::Response {
    async fn maybe_err(self) -> Result<Self, ReqwestErrorWithBody>
    where
        Self: Sized,
    {
        let error = self.error_for_status_ref();
        if let Err(error) = error {
            let body = self.text().await;
            Err(ReqwestErrorWithBody { body, error })
        } else {
            Ok(self)
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ApiError {
    #[error("api request failed: {0}")]
    RequestErrorWithBody(#[from] ReqwestErrorWithBody),

    #[error("api request failed: {0}")]
    RequestFailed(#[from] reqwest::Error),

    #[error("graphql query failed")]
    QueryFailed(Vec<graphql_client::Error>),

    #[error("serialize data to json failed: {0}")]
    SerializeJson(#[from] serde_json::Error),

    #[error("no response from query")]
    NoResponse(String),
}

impl WandB {
    pub fn new(options: BackendOptions) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            format!(
                "Basic {}",
                base64.encode(format!("api:{}", options.api_key))
            )
            .parse()
            .unwrap(),
        );
        headers.insert(reqwest::header::USER_AGENT, "wandb-core".parse().unwrap());
        Self {
            client: reqwest::Client::builder()
                .default_headers(headers)
                .build()
                .unwrap(),
            base_url: options.base_url,
        }
    }
    pub async fn new_run(&self, run_info: upsert_bucket::Variables) -> Result<Run, ApiError> {
        let request_body = UpsertBucket::build_query(run_info);

        let mut res: graphql_client::Response<upsert_bucket::ResponseData> = self
            .client
            .post(format!("{}/graphql", self.base_url))
            .json(&request_body)
            .send()
            .await?
            .maybe_err()
            .await?
            .json()
            .await?;
        if let Some(errors) = &mut res.errors {
            if !errors.is_empty() {
                return Err(ApiError::QueryFailed(errors.drain(..).collect()));
            }
        }
        let bucket = res
            .data
            .ok_or_else(|| ApiError::NoResponse("UpsertBucket query returned empty data".into()))?
            .upsert_bucket
            .ok_or_else(|| {
                ApiError::NoResponse(
                    "UpsertBucket query returned data with no upsert_bucket in response".into(),
                )
            })?
            .bucket
            .ok_or_else(|| {
                ApiError::NoResponse(
                    "UpsertBucket query returned data with no bucket in upsert_bucket".into(),
                )
            })?;
        let project = bucket.project.ok_or_else(|| {
            ApiError::NoResponse(
                "UpsertBucket query returned data with no project in bucket".into(),
            )
        })?;
        // upsertBucket is an upsert, so a run name that already exists resolves
        // to the existing run rather than a new one. Its history file is
        // already `historyLineCount` lines long, and the file stream addresses
        // lines by absolute offset, so start writing after them instead of back
        // at line 0. Absent for a run that was just created.
        let start_offset = bucket.history_line_count.unwrap_or(0).max(0) as u64;
        Ok(Run::new(
            self.base_url.clone(),
            self.client.clone(),
            project.entity.name,
            project.name,
            bucket.name,
            start_offset,
        ))
    }
}

pub struct BackendOptions {
    base_url: String,
    api_key: String,
}

const DEFAULT_API_URL: &str = "https://api.wandb.ai";
impl BackendOptions {
    pub fn new(api_key: String) -> BackendOptions {
        Self {
            base_url: DEFAULT_API_URL.into(),
            api_key,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// Every (path, body) the mock backend was sent.
    type Requests = Arc<Mutex<Vec<(String, String)>>>;

    /// Answer /graphql with the canned reply and everything else with 200 {},
    /// recording every (path, body), until the client hangs up.
    fn serve(stream: TcpStream, graphql_reply: String, requests: Requests) {
        let mut writer = stream.try_clone().expect("clone stream");
        let mut reader = BufReader::new(stream);
        loop {
            let mut request_line = String::new();
            if matches!(reader.read_line(&mut request_line), Ok(0) | Err(_)) {
                return;
            }
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();

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
            let reply = if path == "/graphql" {
                graphql_reply.clone()
            } else {
                "{}".to_string()
            };
            // Record before answering so that a client which has seen its
            // response can rely on the request already being here.
            requests
                .lock()
                .expect("lock requests")
                .push((path, String::from_utf8_lossy(&body).into_owned()));

            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                reply.len(),
                reply
            );
            if writer.write_all(response.as_bytes()).is_err() {
                return;
            }
        }
    }

    fn mock_backend(graphql_reply: String) -> (String, Requests) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let base_url = format!("http://{}", listener.local_addr().expect("listener addr"));
        let requests = Arc::new(Mutex::new(Vec::new()));

        let recorded = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let recorded = Arc::clone(&recorded);
                let graphql_reply = graphql_reply.clone();
                std::thread::spawn(move || serve(stream, graphql_reply, recorded));
            }
        });

        (base_url, requests)
    }

    /// An upsertBucket reply for a run whose history file is already
    /// `history_line_count` lines long. The field is nullable, so pass "null"
    /// for the case where the server reports no count at all.
    fn upsert_reply(history_line_count: &str) -> String {
        format!(
            r#"{{"data":{{"upsertBucket":{{"bucket":{{
                "id":"cnVuOjE=",
                "name":"node-25",
                "displayName":"node-25",
                "description":null,
                "config":null,
                "sweepName":null,
                "project":{{"id":"cHJvajoy","name":"project","entity":{{"id":"ZW50OjM=","name":"entity"}}}},
                "historyLineCount":{history_line_count}
            }},"inserted":false}}}}}}"#
        )
    }

    /// Create a run against the mock, log one row, and report the offset the
    /// row was written at.
    async fn first_history_offset(history_line_count: &str) -> u64 {
        let (base_url, requests) = mock_backend(upsert_reply(history_line_count));
        let wandb = WandB {
            client: reqwest::Client::new(),
            base_url,
        };

        let run = wandb
            .new_run(
                RunInfo::new("project")
                    .name("node-25")
                    .build()
                    .expect("build run info"),
            )
            .await
            .expect("create run");
        run.log((("loss", 0.5),)).await;
        run.finish().await.expect("finish run");

        let requests = requests.lock().expect("lock requests");
        let (path, body) = requests
            .iter()
            .find(|(path, _)| path.ends_with("/file_stream"))
            .expect("a file stream request");
        assert_eq!(path, "/files/entity/project/node-25/file_stream");

        let logged: serde_json::Value = serde_json::from_str(body).expect("file stream body");
        logged["files"]["wandb-history.jsonl"]["offset"]
            .as_u64()
            .expect("history offset")
    }

    #[tokio::test]
    async fn resuming_a_run_appends_after_its_existing_history() {
        // The run already holds lines 0..3999, so the next one is line 4000.
        assert_eq!(first_history_offset("4000").await, 4000);
    }

    #[tokio::test]
    async fn a_new_run_starts_at_the_first_line() {
        assert_eq!(first_history_offset("null").await, 0);
    }
}
