use crate::QPS;
use reqwest::{Client, Url};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};
use rand::Rng;
use tokio::sync::Notify;

pub trait QueryGenerator {
    fn next_query(&mut self) -> String;
}

pub struct RandomLimitSelectStartQueryGenerator;

impl QueryGenerator for RandomLimitSelectStartQueryGenerator {
    fn next_query(&mut self) -> String {
        let limit = rand::thread_rng().gen_range(200..500);
        format!("SELECT * WHERE {{ ?s ?p ?o }} LIMIT {limit}")
    }
}


pub struct RandomReadWorker {
    endpoint: Url,
    client: Client,
    query_gen: Box<dyn QueryGenerator + Send>,
}

impl RandomReadWorker {
    pub fn new(query_gen: Box<dyn QueryGenerator + Send>, endpoint: Url) -> Self {
        Self { endpoint, client: Client::new(), query_gen }
    }

    pub async fn execute(&mut self, stop: Arc<Notify>) -> anyhow::Result<QPS> {
        let n_queries = AtomicUsize::new(0);

        let task = async {
            loop {
                self.client
                    .get(self.endpoint.clone())
                    .query(&[("query", &self.query_gen.next_query())])
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
