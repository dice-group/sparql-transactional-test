use crate::{Query, QPS};
use reqwest::{Client, Url};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};
use tokio::sync::Notify;

pub struct RandomReadWorker {
    endpoint: Url,
    client: Client,
    query: Query,
}

impl RandomReadWorker {
    pub fn new(query: Query, endpoint: Url) -> Self {
        Self { endpoint, client: Client::new(), query }
    }

    pub async fn execute(&self, stop: Arc<Notify>) -> anyhow::Result<QPS> {
        let n_queries = AtomicUsize::new(0);

        let task = async {
            loop {
                self.client
                    .get(self.endpoint.clone())
                    .query(&[("query", &self.query)])
                    .send()
                    .await?
                    .error_for_status()?;

                n_queries.fetch_add(1, Ordering::Relaxed);
            }
        };

        let start_time = Instant::now();

        let res = tokio::select! {
            _ = stop.notified() => Ok(()),
            res = task => res,
        };

        let end_time = Instant::now();

        let qps = n_queries.load(Ordering::Relaxed) as f64 / end_time.duration_since(start_time).as_secs_f64();
        res.map(|_| qps)
    }
}
