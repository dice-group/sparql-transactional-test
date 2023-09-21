use crate::Query;
use rand::{seq::SliceRandom, Rng};
use reqwest::{Client, Url};
use std::{borrow::Cow, io, path::Path, sync::Arc};
use tokio::{
    sync::Notify,
    time::{Duration, Instant},
};

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

    pub async fn execute(&mut self, stop: Arc<Notify>) -> anyhow::Result<(Vec<Duration>, Duration)> {
        let mut query_times = vec![];

        let start_time = Instant::now();

        let worker = async {
            loop {
                let query = self
                    .client
                    .get(self.endpoint.clone())
                    .query(&[("query", &self.query_gen.next_query())])
                    .send();

                let start = Instant::now();
                let resp = query.await?.error_for_status()?.text().await?;
                std::hint::black_box(resp);
                let end = Instant::now();

                query_times.push(end.duration_since(start));
            }
        };

        let success: anyhow::Result<()> = tokio::select! {
            res = worker => res,
            _ = stop.notified() => Ok(())
        };

        success?;

        let end_time = Instant::now();
        Ok((query_times, end_time.duration_since(start_time)))
    }
}
