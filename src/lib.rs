use std::{fmt::Display, future::Future};

use base64::{prelude::BASE64_STANDARD as base64, Engine};
use data_value::LogData;
use gql::{upsert_bucket, UpsertBucket};
use graphql_client::GraphQLQuery;
use run::Run;

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
            config: config,
            project: self.project.into(),
            id: None,
            debug: None,
            description: None,
            display_name: None,
            group_name: None,
            host: None,
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
        Ok(Run::new(
            self.base_url.clone(),
            self.client.clone(),
            project.entity.name,
            project.name,
            bucket.name,
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
