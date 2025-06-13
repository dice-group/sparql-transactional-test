use crate::{
    error::{InvalidStateVerboseInfo, UpdateFailedVerboseInfo, WorkerError},
    WorkerBehaviour,
};
use anyhow::Context;
use reqwest::{header, Client, Response, Url};
use serde::Deserialize;
use std::{fs::File, io, ops::ControlFlow, path::Path};

type DbState = String;
type Subject = String;
type Graph = String;

fn normalize_dbstate(state: DbState) -> DbState {
    let mut lines: Vec<&str> = state.lines().map(|line| line.trim()).collect();

    lines.sort();

    lines.into_iter().flat_map(|line| [line, "\n"]).collect()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum OperationKind {
    InsertData,
    DeleteData,
    GspPost,
    GspPut,
    GspDelete,
}

#[derive(Debug, Deserialize)]
struct UpdateOperation {
    subject: Subject,
    operation: OperationKind,
    triples: Graph,
    validation_default: DbState,
    validation_named: DbState,
}

impl UpdateOperation {
    fn normalize(self) -> Self {
        Self {
            validation_default: normalize_dbstate(self.validation_default),
            validation_named: normalize_dbstate(self.validation_named),
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
    fn make_verify_query(subject: &str) -> String {
        format!("CONSTRUCT {{ {subject} ?p ?o }} WHERE {{ {subject} ?p ?o . FILTER(!isBlank(?o)) }}")
    }

    fn make_named_verify_query(subject: &str) -> String {
        format!(
            "CONSTRUCT {{ {subject} ?p ?o }} WHERE {{ GRAPH {subject} {{ {subject} ?p ?o . FILTER(!isBlank(?o)) }} }}"
        )
    }

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

    async fn read_current_state_impl(&self, query: String) -> reqwest::Result<ControlFlow<DbState>> {
        let resp = self
            .client
            .get(self.query_endpoint.clone())
            .header(header::ACCEPT, "application/n-triples")
            .query(&[("query", query)])
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

    async fn read_current_state(&self, subject: &str) -> reqwest::Result<ControlFlow<(DbState, DbState)>> {
        let (default_state, named_state) = tokio::join!(
            self.read_current_state_impl(Self::make_verify_query(subject)),
            self.read_current_state_impl(Self::make_named_verify_query(subject))
        );

        let default_state = default_state?;
        let named_state = named_state?;

        Ok(match (default_state, named_state) {
            (ControlFlow::Break(default), ControlFlow::Break(named)) => ControlFlow::Break((default, named)),
            (_, _) => ControlFlow::Continue(()),
        })
    }

    async fn issue_update(
        &self,
        UpdateOperation { subject, operation, triples, .. }: &UpdateOperation,
    ) -> reqwest::Result<ControlFlow<()>> {
        let resp = match operation {
            OperationKind::DeleteData => {
                let update = format!("DELETE DATA {{ {triples} }}");

                self.client
                    .post(self.update_endpoint.clone())
                    .header(header::CONTENT_TYPE, "application/sparql-update")
                    .body(update)
                    .send()
                    .await
            },
            OperationKind::InsertData => {
                let update = format!("INSERT DATA {{ {triples} }}; INSERT DATA {{ GRAPH {subject} {{ {triples} }} }}");

                self.client
                    .post(self.update_endpoint.clone())
                    .header(header::CONTENT_TYPE, "application/sparql-update")
                    .body(update)
                    .send()
                    .await
            },
            OperationKind::GspDelete => {
                self.client
                    .delete(self.graph_store_endpoint.clone())
                    .query(&[("graph", subject.trim_start_matches('<').trim_end_matches('>'))])
                    .send()
                    .await
            },
            OperationKind::GspPost => {
                self.client
                    .post(self.graph_store_endpoint.clone())
                    .header(header::CONTENT_TYPE, "application/n-triples")
                    .query(&[("graph", subject.trim_start_matches('<').trim_end_matches('>'))])
                    .body(triples.clone())
                    .send()
                    .await
            },
            OperationKind::GspPut => {
                self.client
                    .put(self.graph_store_endpoint.clone())
                    .header(header::CONTENT_TYPE, "application/n-triples")
                    .query(&[(
                        "graph",
                        subject.trim_start_matches('<').trim_end_matches('>').to_owned(),
                    )])
                    .body(triples.clone())
                    .send()
                    .await
            },
        };

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
                match self.read_current_state(&update.subject).await {
                    Ok(ControlFlow::Continue(())) => continue,
                    Ok(ControlFlow::Break((actual_default_state, actual_named_state)))
                        if actual_default_state == update.validation_default
                            && actual_named_state == update.validation_named =>
                    {
                        break Ok(())
                    },
                    Ok(ControlFlow::Break(actual_state)) => {
                        break Err(WorkerError::InvalidState {
                            update_id: id,
                            verbose_info: if self.verbose {
                                Some(InvalidStateVerboseInfo {
                                    expected: (update.validation_default.clone(), update.validation_named.clone()),
                                    actual: actual_state,
                                })
                            } else {
                                None
                            },
                        })
                    },
                    Err(err) => {
                        break Err(WorkerError::UpdateVerifyFailed {
                            update_id: id,
                            subject: update.subject.to_owned(),
                            err,
                        })
                    },
                }
            }?;
        }

        Ok(())
    }
}
