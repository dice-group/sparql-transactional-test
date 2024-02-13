use crate::{
    error::{InvalidStateVerboseInfo, UpdateFailedVerboseInfo, WorkerError},
    Query,
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
}

impl UpdateWorker {
    fn make_verify_query(subject: &str) -> String {
        format!("CONSTRUCT {{ {subject} ?p ?o }} WHERE {{ {subject} ?p ?o . FILTER(!isBlank(?o)) }}")
    }

    pub fn new(base_dir: &Path, query_endpoint: Url, update_endpoint: Url, verbose: bool) -> anyhow::Result<Self> {
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
        })
    }

    async fn read_current_state(&self, subject: &str) -> reqwest::Result<DbState> {
        let state = self
            .client
            .get(self.query_endpoint.clone())
            .header(header::ACCEPT, "application/n-triples")
            .query(&[("query", Self::make_verify_query(subject))])
            .send()
            .await?
            .text()
            .await?;

        let mut ret: DbState = state.lines().map(ToOwned::to_owned).collect();
        ret.sort();

        Ok(ret)
    }

    async fn issue_update(&self, update: &str) -> reqwest::Result<()> {
        self.client
            .post(self.update_endpoint.clone())
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(update.to_owned())
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    pub async fn execute(&self) -> Result<(), WorkerError> {
        for (id, (subject, update, expected_state)) in self.queries.iter().enumerate() {
            self.issue_update(update)
                .await
                .map_err(|err| WorkerError::UpdateFailed {
                    update_id: id,
                    err,
                    verbose_info: if self.verbose {
                        Some(UpdateFailedVerboseInfo { query: update.clone() })
                    } else {
                        None
                    },
                })?;

            let actual_state = self
                .read_current_state(subject)
                .await
                .map_err(|err| WorkerError::UpdateVerifyFailed { update_id: id, subject: subject.to_owned(), err })?;

            if &actual_state != expected_state {
                return Err(WorkerError::InvalidState {
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
                });
            }
        }

        Ok(())
    }
}
