use crate::{
    error::{InvalidStateVerboseInfo, UpdateFailedVerboseInfo, WorkerError},
    Query, WorkerBehaviour,
};
use anyhow::Context;
use reqwest::{header, Client, Url};
use std::path::Path;

type DbState = Vec<String>;
type Subject = String;

pub struct UpdateWorker {
    query_endpoint: Url,
    update_endpoint: Url,
    client: Client,
    queries: Vec<(Subject, Query, DbState)>,
    verbose: bool,
    behav: WorkerBehaviour,
}

impl UpdateWorker {
    fn make_verify_query(subject: &str) -> String {
        format!("CONSTRUCT {{ {subject} ?p ?o }} WHERE {{ {subject} ?p ?o . FILTER(!isBlank(?o)) }}")
    }

    pub fn new(
        base_dir: &Path,
        query_endpoint: Url,
        update_endpoint: Url,
        verbose: bool,
        behav: WorkerBehaviour,
    ) -> anyhow::Result<Self> {
        let mut queries = Vec::new();

        for op in 1.. {
            let subject = base_dir.join(format!("op_{op}.txt"));
            let update = base_dir.join(format!("op_{op}.ru"));
            let update_result = base_dir.join(format!("op_{op}.nt"));

            if !subject.exists() && !update.exists() && !update_result.exists() {
                break;
            }

            let subject = std::fs::read_to_string(&subject).context(format!(
                "Unable to read subject of query {op} from {}",
                subject.display()
            ))?;

            let update = std::fs::read_to_string(&update)
                .context(format!("Unable to read update of query {op} from {}", update.display()))?;

            let mut update_result: Vec<_> = std::fs::read_to_string(&update_result)
                .context(format!(
                    "Unable to read result of query {op} from {}",
                    update_result.display()
                ))?
                .lines()
                .map(ToOwned::to_owned)
                .collect();

            update_result.sort();

            queries.push((subject, update, update_result));
        }

        Ok(Self {
            query_endpoint,
            update_endpoint,
            client: Client::new(),
            queries,
            verbose,
            behav,
        })
    }

    async fn read_current_state(&self, subject: &str) -> reqwest::Result<Option<DbState>> {
        let resp = self
            .client
            .get(self.query_endpoint.clone())
            .header(header::ACCEPT, "application/n-triples")
            .query(&[("query", Self::make_verify_query(subject))])
            .send()
            .await;

        match resp {
            Ok(resp) => {
                let resp = resp.error_for_status()?;
                match resp.text().await {
                    Ok(state) => {
                        let mut ret: DbState = state.lines().map(ToOwned::to_owned).collect();
                        ret.sort();

                        Ok(Some(ret))
                    },
                    Err(_) if self.behav == WorkerBehaviour::IgnoreConnectionError => Ok(None),
                    Err(e) => Err(e),
                }
            },
            Err(_) if self.behav == WorkerBehaviour::IgnoreConnectionError => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn issue_update(&self, update: &str) -> reqwest::Result<Option<()>> {
        let resp = self
            .client
            .post(self.update_endpoint.clone())
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(update.to_owned())
            .send()
            .await;

        match resp {
            Ok(resp) => {
                resp.error_for_status()?;
                Ok(Some(()))
            },
            Err(_) if self.behav == WorkerBehaviour::IgnoreConnectionError => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub async fn execute(&self) -> Result<(), WorkerError> {
        for (id, (subject, update, expected_state)) in self.queries.iter().enumerate() {
            loop {
                match self.issue_update(update).await {
                    Ok(None) => continue,
                    Ok(Some(_)) => break Ok(()),
                    Err(err) => {
                        break Err(WorkerError::UpdateFailed {
                            update_id: id,
                            err,
                            verbose_info: if self.verbose {
                                Some(UpdateFailedVerboseInfo { query: update.clone() })
                            } else {
                                None
                            },
                        })
                    },
                }
            }?;

            loop {
                match self.read_current_state(subject).await {
                    Ok(None) => continue,
                    Ok(Some(actual_state)) if &actual_state == expected_state => break Ok(()),
                    Ok(Some(actual_state)) => {
                        break Err(WorkerError::InvalidState {
                            update_id: id + 1,
                            verbose_info: if self.verbose {
                                Some(InvalidStateVerboseInfo {
                                    update: update.to_owned(),
                                    expected: expected_state.join("\n"),
                                    actual: actual_state.join("\n"),
                                })
                            } else {
                                None
                            },
                        })
                    },
                    Err(err) => {
                        break Err(WorkerError::UpdateVerifyFailed { update_id: id, subject: subject.to_owned(), err })
                    },
                }
            }?;
        }

        Ok(())
    }
}
