use crate::{Query, QPS};
use rand::{seq::SliceRandom, Rng};
use reqwest::{Client, Url};
use std::{borrow::Cow, collections::BTreeMap, io, path::Path, sync::Arc};
use tokio::{sync::Notify, time::Instant};

pub trait QueryGenerator {
    fn next_query(&mut self) -> (Option<usize>, Cow<str>);
}

#[derive(Copy, Clone)]
pub struct RandomLimitSelectStartQueryGenerator;

impl QueryGenerator for RandomLimitSelectStartQueryGenerator {
    fn next_query(&mut self) -> (Option<usize>, Cow<str>) {
        let limit = rand::thread_rng().gen_range(200..500);
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
    fn next_query(&mut self) -> (Option<usize>, Cow<str>) {
        let cur_ix = self.ix;
        if cur_ix == 0 {
            self.queries.shuffle(&mut rand::thread_rng());
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
}

impl RandomReadWorker {
    pub fn new(query_gen: Box<dyn QueryGenerator + Send>, endpoint: Url) -> Self {
        let client = Client::builder().tcp_nodelay(true).build().unwrap();

        Self { endpoint, client, query_gen }
    }

    pub async fn execute(&mut self, stop: Arc<Notify>) -> anyhow::Result<BTreeMap<usize, QPS>> {
        let mut query_qps: BTreeMap<_, Vec<QPS>> = Default::default();

        let worker = async {
            loop {
                let (qid, q) = self.query_gen.next_query();

                let query = self.client.get(self.endpoint.clone()).query(&[("query", &q)]).send();

                let start = Instant::now();
                let resp = query.await?.error_for_status()?.text().await?;
                std::hint::black_box(resp);
                let end = Instant::now();

                if let Some(id) = qid {
                    query_qps
                        .entry(id)
                        .or_default()
                        .push(1.0 / end.duration_since(start).as_secs_f64());
                }
            }
        };

        let success: anyhow::Result<()> = tokio::select! {
            res = worker => res,
            _ = stop.notified() => Ok(())
        };

        success?;

        // for each query
        // let qpss(query) be a list of the individual qps measurements for query
        // then qps(query) = sum(qpss(query)) / len(qpss(query))
        Ok(query_qps
            .into_iter()
            .map(|(q, qpss)| (q, qpss.iter().sum::<QPS>() / qpss.len() as f64))
            .collect())
    }
}
