use crate::{DbState, Query, Subject};
use anyhow::{Context, ensure};
use reqwest::{header, Client, Url};
use std::{
    fmt::{Display, Formatter},
    path::Path,
};

#[derive(Debug)]
pub struct DiffError {
    pub worker_id: usize,
    pub update_id: usize,
    pub update: String,
    pub expected: String,
    pub actual: String,
}

fn format_insert_or_delete_data(f: &mut Formatter<'_>, q: &str) -> std::fmt::Result {
    let (head, body) = q.split_once("{ <").unwrap();
    let (body, _) = body.rsplit_once(". }").unwrap();

    writeln!(f, "{head}{{")?;

    for triple in body.split(". <") {
        writeln!(f, "    <{triple}.")?;
    }

    writeln!(f, "}}")?;
    Ok(())
}

impl Display for DiffError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Unexpected result from worker {} at update {}\n\nquery:\n",
            self.worker_id, self.update_id
        )?;

        if self.update.starts_with("INSERT DATA") || self.update.starts_with("DELETE DATA") {
            format_insert_or_delete_data(f, &self.update)?;
        } else {
            writeln!(f, "{}", self.update)?;
        }

        writeln!(f, "\nexpected:\n{}", self.expected)?;
        writeln!(f, "\nactual:\n{}", self.actual)?;

        writeln!(f, "\ndiff:\n{}", prettydiff::diff_lines(&self.expected, &self.actual))
    }
}

pub struct UpdateWorker {
    id: usize,
    query_endpoint: Url,
    update_endpoint: Url,
    client: Client,
    queries: Vec<(Subject, Query, DbState)>,
}

impl UpdateWorker {
    fn make_verify_query(subject: &str) -> String {
        format!("CONSTRUCT {{ {subject} ?p ?o }} WHERE {{ {subject} ?p ?o . FILTER(!isBlank(?o)) }}")
    }

    pub fn new(id: usize, base_dir: &Path, query_endpoint: Url, update_endpoint: Url) -> anyhow::Result<Self> {
        let mut queries = Vec::new();

        for op in 1.. {
            let subject = base_dir.join(format!("op_{op}.txt"));
            let update = base_dir.join(format!("op_{op}.ru"));
            let update_result = base_dir.join(format!("op_{op}.nt"));

            if !subject.exists() && !update.exists() && !update_result.exists() {
                break;
            }

            let subject = std::fs::read_to_string(&subject)
                .context(format!("Worker {id} is unable to read subject of query {op} from {}", subject.display()))?;

            let update = std::fs::read_to_string(&update)
                .context(format!("Worker {id} is unable to read update of query {op} from {}", update.display()))?;

            let mut update_result: Vec<_> = std::fs::read_to_string(&update_result)
                .context(format!("Worker {id} is unable to read result of query {op} from {}", update_result.display()))?
                .lines()
                .map(ToOwned::to_owned)
                .collect();

            update_result.sort();

            queries.push((subject, update, update_result));
        }

        Ok(Self { id, query_endpoint, update_endpoint, client: Client::new(), queries })
    }

    async fn read_current_state(&self, subject: &str) -> anyhow::Result<DbState> {
        let state = self
            .client
            .get(self.query_endpoint.clone())
            .query(&[("query", Self::make_verify_query(subject))])
            .send()
            .await?
            .text()
            .await?;

        let mut ret: DbState = state.lines().map(ToOwned::to_owned).collect();
        ret.sort();

        Ok(ret)
    }

    async fn issue_update(&self, update: &str) -> anyhow::Result<()> {
        self.client
            .post(self.update_endpoint.clone())
            .header(header::CONTENT_TYPE, "application/sparql-update")
            .body(update.to_owned())
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    pub async fn execute(&self) -> anyhow::Result<()> {
        for (id, (subject, update, expected_state)) in self.queries.iter().enumerate() {
            self.issue_update(update).await?;

            let actual_state = self.read_current_state(subject).await?;

            ensure!(
                &actual_state == expected_state,
                DiffError {
                    worker_id: self.id,
                    update_id: id + 1,
                    update: update.to_owned(),
                    expected: expected_state.join("\n"),
                    actual: actual_state.join("\n")
                }
            );
        }

        Ok(())
    }
}
