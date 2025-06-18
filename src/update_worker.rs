use crate::{
    error::{InvalidStateVerboseInfo, UpdateFailedVerboseInfo, WorkerError},
    Query, WorkerBehaviour,
};
use anyhow::Context;
use reqwest::{header, Client, Url};
use serde::Deserialize;
use std::{collections::HashMap, fs::File, io, ops::ControlFlow, path::Path};

type DbState = String;

fn normalize_dbstate(state: DbState) -> DbState {
    let mut lines: Vec<&str> = state.lines().map(|line| line.trim()).collect();

    lines.sort();

    lines.into_iter().flat_map(|line| [line, "\n"]).collect()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum Endpoint {
    Update,
    Gsp,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum Method {
    Post,
    Put,
    Delete,
}

#[derive(Debug, Deserialize)]
struct Validate {
    query: Query,
    expected: DbState,
}

#[derive(Debug, Deserialize)]
struct UpdateOperation {
    endpoint: Endpoint,
    query_params: HashMap<String, String>,
    headers: HashMap<String, String>,
    method: Method,
    body: String,
    validate: Validate,
}

impl UpdateOperation {
    fn normalize(self) -> Self {
        Self {
            validate: Validate {
                query: self.validate.query,
                expected: normalize_dbstate(self.validate.expected),
            },
            ..self
        }
    }
}

pub struct UpdateWorker {
    query_endpoint: Url,
    update_endpoint: Url,
    graph_store_endpoint: Url,
    client: Client,
    queries: Vec<UpdateOperation>,
    verbose: bool,
    behav: WorkerBehaviour,
}

impl UpdateWorker {
    pub fn new(
        base_dir: &Path,
        query_endpoint: Url,
        update_endpoint: Url,
        graph_store_endpoint: Url,
        verbose: bool,
        behav: WorkerBehaviour,
    ) -> anyhow::Result<Self> {
        let mut queries = Vec::new();

        for op in 0.. {
            let file = match File::open(base_dir.join(format!("op_{op}.json"))) {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => break,
                Err(e) => return Err(e).context(format!("Unable to open op {op}")),
            };

            let update: UpdateOperation =
                serde_json::from_reader(file).context(format!("Unable to deserialize operation {op}"))?;

            queries.push(update.normalize());
        }

        anyhow::ensure!(
            !queries.is_empty(),
            "Did not find any operations for update worker in {}",
            base_dir.display()
        );

        Ok(Self {
            query_endpoint,
            update_endpoint,
            graph_store_endpoint,
            client: Client::new(),
            queries,
            verbose,
            behav,
        })
    }
    async fn read_current_state(
        &self,
        UpdateOperation { validate, .. }: &UpdateOperation,
    ) -> reqwest::Result<ControlFlow<DbState>> {
        let resp = self
            .client
            .get(self.query_endpoint.clone())
            .header(header::ACCEPT, "application/n-triples")
            .query(&[("query", validate.query.clone())])
            .send()
            .await;

        match resp {
            Ok(resp) => {
                let resp = resp.error_for_status()?;
                match resp.text().await {
                    Ok(state) => Ok(ControlFlow::Break(normalize_dbstate(state))),
                    Err(_) if self.behav == WorkerBehaviour::IgnoreConnectionError => Ok(ControlFlow::Continue(())),
                    Err(e) => Err(e),
                }
            },
            Err(_) if self.behav == WorkerBehaviour::IgnoreConnectionError => Ok(ControlFlow::Continue(())),
            Err(e) => Err(e),
        }
    }

    async fn issue_update(&self, operation: &UpdateOperation) -> reqwest::Result<ControlFlow<()>> {
        let endpoint = match operation.endpoint {
            Endpoint::Update => &self.update_endpoint,
            Endpoint::Gsp => &self.graph_store_endpoint,
        };

        let req = match operation.method {
            Method::Post => self.client.post(endpoint.clone()),
            Method::Put => self.client.put(endpoint.clone()),
            Method::Delete => self.client.delete(endpoint.clone()),
        };

        let resp = req
            .headers((&operation.headers).try_into().unwrap())
            .query(&operation.query_params.clone())
            .body(operation.body.clone())
            .send()
            .await;

        match resp {
            Ok(resp) => {
                resp.error_for_status()?;
                Ok(ControlFlow::Break(()))
            },
            Err(_) if self.behav == WorkerBehaviour::IgnoreConnectionError => Ok(ControlFlow::Continue(())),
            Err(e) => Err(e),
        }
    }

    pub async fn execute(&self) -> Result<(), WorkerError> {
        for (id, update) in self.queries.iter().enumerate() {
            loop {
                match self.issue_update(update).await {
                    Ok(ControlFlow::Continue(())) => continue,
                    Ok(ControlFlow::Break(())) => break Ok(()),
                    Err(err) => {
                        break Err(WorkerError::UpdateFailed {
                            update_id: id,
                            err,
                            verbose_info: if self.verbose {
                                Some(UpdateFailedVerboseInfo { query: format!("{update:?}") })
                            } else {
                                None
                            },
                        })
                    },
                }
            }?;

            loop {
                match self.read_current_state(&update).await {
                    Ok(ControlFlow::Continue(())) => continue,
                    Ok(ControlFlow::Break(actual_state)) if actual_state == update.validate.expected => break Ok(()),
                    Ok(ControlFlow::Break(actual_state)) => {
                        break Err(WorkerError::InvalidState {
                            update_id: id,
                            verbose_info: if self.verbose {
                                Some(InvalidStateVerboseInfo {
                                    query: update.validate.query.clone(),
                                    expected: update.validate.expected.clone(),
                                    actual: actual_state,
                                })
                            } else {
                                None
                            },
                        })
                    },
                    Err(err) => break Err(WorkerError::UpdateVerifyFailed { update_id: id, err }),
                }
            }?;
        }

        Ok(())
    }
}
