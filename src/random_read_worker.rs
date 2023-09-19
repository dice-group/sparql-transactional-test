use crate::{Query, QPS};
use rand::Rng;
use reqwest::{Client, Url};
use std::{
    borrow::Cow,
    io,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Instant,
};
use rand::seq::SliceRandom;
use tokio::sync::Notify;

pub trait QueryGenerator {
    fn next_query(&mut self) -> Cow<str>;
}

#[derive(Copy, Clone)]
pub struct RandomLimitSelectStartQueryGenerator;

impl QueryGenerator for RandomLimitSelectStartQueryGenerator {
    fn next_query(&mut self) -> Cow<str> {
        let limit = rand::thread_rng().gen_range(200..500);
        Cow::Owned(format!("SELECT * WHERE {{ ?s ?p ?o }} LIMIT {limit}"))
    }
}

#[derive(Clone)]
pub struct FileSourceQueryGenerator {
    queries: Vec<Query>,
    ix: usize,
}

impl FileSourceQueryGenerator {
    pub fn new<P: AsRef<Path>>(query_file: P) -> io::Result<Self> {
        let query_file = query_file.as_ref();

        let queries = std::fs::read_to_string(query_file)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(ToOwned::to_owned)
            .collect();

        Ok(Self { queries, ix: 0 })
    }
}

impl QueryGenerator for FileSourceQueryGenerator {
    fn next_query(&mut self) -> Cow<str> {
        let cur_ix = self.ix;
        if cur_ix == 0 {
            self.queries.shuffle(&mut rand::thread_rng());
        }

        self.ix = (self.ix + 1) % self.queries.len();

        Cow::Borrowed(&self.queries[cur_ix])
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
