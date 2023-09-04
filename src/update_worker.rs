use crate::{DbState, Query};
use anyhow::ensure;
use reqwest::{Client, Url};
use std::{collections::BTreeMap, path::Path};

pub struct UpdateWorker {
    query_endpoint: Url,
    update_endpoint: Url,
    client: Client,
    update_queries: Vec<Query>,
    states: Vec<DbState>,
    verify_query: Query,
}

impl UpdateWorker {
    pub fn new(base_dir: &Path, query_endpoint: Url, update_endpoint: Url) -> anyhow::Result<Self> {
        let mut tmp = BTreeMap::new();

        for update in std::fs::read_dir(base_dir.join("updates"))? {
            let update = update?;
            let path = update.path();

            let query = std::fs::read_to_string(&path)?;
            tmp.insert(path, query);
        }

        let verify_query = std::fs::read_to_string(base_dir.join("verify.sparql"))?;

        Ok(Self {
            query_endpoint,
            update_endpoint,
            client: Client::new(),
            update_queries: tmp.into_iter().map(|(_, update)| update).collect(),
            states: Vec::new(),
            verify_query,
        })
    }

    async fn read_current_state(&self) -> anyhow::Result<DbState> {
        let state = self
            .client
            .get(self.query_endpoint.clone())
            .query(&[("query", &self.verify_query)])
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
            .body(update.to_owned())
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    pub async fn prepare(&mut self) -> anyhow::Result<()> {
        assert!(self.states.is_empty());

        let init_state = self.read_current_state().await?;
        self.states.push(init_state);

        for update in &self.update_queries {
            self.issue_update(update).await?;
            let state = self.read_current_state().await?;
            self.states.push(state);
        }

        Ok(())
    }

    pub async fn execute(&self) -> anyhow::Result<()> {
        assert!(!self.states.is_empty());

        {
            let expected = &self.states[0];
            let actual = self.read_current_state().await?;
            ensure!(
                &actual == expected,
                "expected initial state:\n{expected:#?}\n\nbut got state:\n{actual:#?}"
            );
        }

        for (id, update) in self.update_queries.iter().enumerate() {
            self.issue_update(update).await?;

            let expected = &self.states[id + 1];
            let actual = self.read_current_state().await?;
            ensure!(
                &actual == expected,
                "after update ({id}):\n{update}\n\nexpected state:\n{expected:#?}\n\nbut got state:\n{actual:#?}"
            );
        }

        Ok(())
    }
}
