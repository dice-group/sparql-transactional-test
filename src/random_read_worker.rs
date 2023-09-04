use crate::Query;
use reqwest::{Client, Url};
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;

pub struct RandomReadWorker {
    endpoint: Url,
    client: Client,
    query: Query,
    period: Duration,
}

impl RandomReadWorker {
    pub fn new(query: Query, period: Duration, endpoint: Url) -> Self {
        Self { endpoint, client: Client::new(), query, period }
    }

    pub async fn execute(&self, stop: Arc<Notify>) -> anyhow::Result<()> {
        let task = async move {
            loop {
                self.client
                    .get(self.endpoint.clone())
                    .query(&[("query", &self.query)])
                    .send()
                    .await?
                    .error_for_status()?;

                tokio::time::sleep(self.period).await;
            }
        };

        tokio::select! {
            _ = stop.notified() => Ok(()),
            res = task => res,
        }
    }
}
