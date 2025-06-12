use crate::{error::WorkerError, Query, WorkerBehaviour, QPS};
use rand::{seq::SliceRandom, Rng};
use reqwest::{Client, Response, Url};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    future::Future,
    io,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Notify;

pub trait QueryGenerator {
    fn next_query(&mut self) -> (Option<usize>, Cow<str>);
}

#[derive(Copy, Clone)]
pub struct RandomLimitSelectStartQueryGenerator;

impl QueryGenerator for RandomLimitSelectStartQueryGenerator {
    fn next_query(&mut self) -> (Option<usize>, Cow<'_, str>) {
        let limit = rand::rng().random_range(200..500);
        (None, Cow::Owned(format!("SELECT * WHERE {{ ?s ?p ?o }} LIMIT {limit}")))
    }
}

#[derive(Clone)]
pub struct FileSourceQueryGenerator {
    queries_original_order: Vec<Query>,
    queries: Vec<Query>,
    ix: usize,
}

impl FileSourceQueryGenerator {
    pub fn new<P: AsRef<Path>>(query_file: P) -> io::Result<Self> {
        let query_file = query_file.as_ref();

        let queries: Vec<_> = std::fs::read_to_string(query_file)?
            .lines()
            .filter(|l| !l.is_empty())
            .map(ToOwned::to_owned)
            .collect();

        Ok(Self { queries_original_order: queries.clone(), queries, ix: 0 })
    }
}

impl QueryGenerator for FileSourceQueryGenerator {
    fn next_query(&mut self) -> (Option<usize>, Cow<'_, str>) {
        let cur_ix = self.ix;
        if cur_ix == 0 {
            self.queries.shuffle(&mut rand::rng());
        }

        self.ix = (self.ix + 1) % self.queries.len();

        let ret_query = &self.queries[cur_ix];
        let orig_ix = self.queries_original_order.iter().position(|q| q == ret_query).unwrap();

        (Some(orig_ix), Cow::Borrowed(ret_query))
    }
}

pub struct RandomReadWorker {
    endpoint: Url,
    client: Client,
    query_gen: Box<dyn QueryGenerator + Send>,
    behav: WorkerBehaviour,
}

impl RandomReadWorker {
    pub fn new(query_gen: Box<dyn QueryGenerator + Send>, endpoint: Url, behav: WorkerBehaviour) -> Self {
        let client = Client::builder().tcp_nodelay(true).build().unwrap();

        Self { endpoint, client, query_gen, behav }
    }

    async fn measure_query(
        behav: WorkerBehaviour,
        query: impl Future<Output = reqwest::Result<Response>>,
    ) -> reqwest::Result<Option<Duration>> {
        let start = Instant::now();

        match query.await {
            Ok(resp) => {
                let resp = resp.error_for_status()?;

                match resp.text().await {
                    Ok(text) => {
                        std::hint::black_box(text);
                        let end = Instant::now();
                        Ok(Some(end.duration_since(start)))
                    },
                    Err(_) if behav == WorkerBehaviour::IgnoreConnectionError => Ok(None),
                    Err(e) => Err(e),
                }
            },
            Err(_) if behav == WorkerBehaviour::IgnoreConnectionError => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub async fn execute(&mut self, stop: Arc<Notify>) -> Result<BTreeMap<usize, QPS>, WorkerError> {
        let mut query_timings: BTreeMap<_, Vec<Duration>> = Default::default();

        let worker = async {
            loop {
                let (qid, q) = self.query_gen.next_query();

                let qfut = self.client.get(self.endpoint.clone()).query(&[("query", &q)]).send();
                let dur = Self::measure_query(self.behav, qfut)
                    .await
                    .map_err(|e| WorkerError::ReadFailed { query: q.into_owned(), err: e })?;

                if let (Some(id), Some(dur)) = (qid, dur) {
                    query_timings.entry(id).or_default().push(dur);
                }
            }
        };

        let success = tokio::select! {
            res = worker => res,
            _ = stop.notified() => Ok(())
        };

        success?;

        Ok(query_timings
            .into_iter()
            .map(|(qid, durations)| {
                let avg_duration_secs = durations.iter().sum::<Duration>().as_secs_f64() / durations.len() as f64;
                let qps = 1.0 / avg_duration_secs;

                (qid, qps)
            })
            .collect())
    }
}
