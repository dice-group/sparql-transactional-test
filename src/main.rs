mod random_read_worker;
mod update_worker;

use crate::random_read_worker::{FileSourceQueryGenerator, QueryGenerator};
use clap::Parser;
use random_read_worker::{RandomLimitSelectStartQueryGenerator, RandomReadWorker};
use reqwest::Url;
use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Barrier, Notify};
use update_worker::UpdateWorker;

type DbState = Vec<String>;
type Query = String;
type Subject = String;
type QPS = f64;
type AvgQPS = f64;

#[derive(serde::Serialize)]
struct Datapoint {
    reader: usize,
    query_id: usize,
    qps: f64,
}

enum WorkerType {
    Update,
    Reader,
}

#[derive(Parser)]
struct ReaderOpts {
    /// Number of random readers to spawn
    #[clap(short = 'r', long)]
    num_random_read_workers: usize,

    /// Optionally, a file with SPARQL queries that the readers should use (one query per line)
    /// If not provided readers will simply run `SELECT *` with varying limits
    #[clap(short = 'q', long)]
    random_read_workers_query_file: Option<PathBuf>,
}

#[derive(Parser)]
enum SubCommand {
    /// A read-only stress test that measures QPS
    Stress {
        #[clap(flatten)]
        reader_opts: ReaderOpts,

        /// The number of seconds the stress test should last
        #[clap(short = 't', long)]
        duration_s: u64,

        /// If set, will output a csv file to stdout with per query timings for each reader
        #[clap(long)]
        output_per_query_qps_csv: bool,

        /// URL to SPARQL (read) endpoint to stress
        query_endpoint: Url,
    },
    /// A read-write workload that checks for correctness of concurrent updates and reads
    Verify {
        #[clap(flatten)]
        reader_opts: ReaderOpts,

        /// Number of update workers to spawn
        #[clap(short = 'w', long)]
        num_update_workers: usize,

        /// Path to the directory that contains the information for the updaters
        #[clap(short = 'Q', long)]
        update_query_dir: PathBuf,

        /// URL to SPARQL endpoint for the random readers
        query_endpoint: Url,

        /// URL to SPARQL endpoint for the updaters
        update_endpoint: Url,
    },
}

#[derive(Parser)]
struct Command {
    /// Do not color log output
    #[clap(long)]
    no_color: bool,

    #[clap(subcommand)]
    sub: SubCommand,
}

fn make_random_readers(
    query_endpoint: &Url,
    ReaderOpts { num_random_read_workers, random_read_workers_query_file }: &ReaderOpts,
) -> anyhow::Result<Vec<RandomReadWorker>> {
    let mut random_read_workers = Vec::with_capacity(*num_random_read_workers);

    for _ in 0..*num_random_read_workers {
        let query_gen: Box<dyn QueryGenerator + Send> = if let Some(query_file) = &random_read_workers_query_file {
            Box::new(FileSourceQueryGenerator::new(query_file)?)
        } else {
            Box::new(RandomLimitSelectStartQueryGenerator)
        };

        let w = RandomReadWorker::new(query_gen, query_endpoint.clone());
        random_read_workers.push(w);
    }

    Ok(random_read_workers)
}

fn make_update_workers(
    query_endpoint: &Url,
    update_endpoint: &Url,
    num_update_workers: usize,
    query_dir: &Path,
) -> anyhow::Result<Vec<UpdateWorker>> {
    let mut update_workers = Vec::with_capacity(num_update_workers);
    for worker in 1..=num_update_workers {
        let w = UpdateWorker::new(
            worker,
            Path::new(&query_dir.join(format!("worker_{worker}"))),
            query_endpoint.clone(),
            update_endpoint.clone(),
        )?;

        update_workers.push(w);
    }

    Ok(update_workers)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts: Command = Command::parse();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal() && !opts.no_color)
        .init();

    let (update_workers, random_read_workers) = match &opts.sub {
        SubCommand::Stress { reader_opts, query_endpoint, .. } => {
            (vec![], make_random_readers(query_endpoint, reader_opts)?)
        },
        SubCommand::Verify {
            reader_opts,
            num_update_workers,
            update_query_dir,
            query_endpoint,
            update_endpoint,
        } => (
            make_update_workers(query_endpoint, update_endpoint, *num_update_workers, update_query_dir)?,
            make_random_readers(query_endpoint, reader_opts)?,
        ),
    };

    let num_update_workers = update_workers.len();
    let num_random_read_workers = random_read_workers.len();

    let start_barrier = Arc::new(Barrier::new(num_update_workers + num_random_read_workers + 1));
    let (finished_tx, mut finished_rx) = tokio::sync::mpsc::channel(num_update_workers + num_random_read_workers);

    for (worker_id, update_worker) in update_workers.into_iter().enumerate() {
        let start_barrier = start_barrier.clone();
        let finished_tx = finished_tx.clone();

        tokio::spawn(async move {
            start_barrier.wait().await;
            tracing::info!("Starting update worker {}", worker_id + 1);

            let res = update_worker.execute().await;
            finished_tx
                .send((WorkerType::Update, worker_id, res.map(|_| Default::default())))
                .await
                .unwrap();
        });
    }

    let stop_notify = Arc::new(Notify::new());

    for (worker_id, mut rr_worker) in random_read_workers.into_iter().enumerate() {
        let start_barrier = start_barrier.clone();
        let finished_tx = finished_tx.clone();
        let stop_notify = stop_notify.clone();

        tokio::spawn(async move {
            start_barrier.wait().await;
            tracing::info!("Starting random read worker {}", worker_id + 1);

            let res = rr_worker.execute(stop_notify).await;
            finished_tx.send((WorkerType::Reader, worker_id, res)).await.unwrap();
        });
    }

    start_barrier.wait().await;
    let start_time = tokio::time::Instant::now();

    if let SubCommand::Stress { duration_s, .. } = opts.sub {
        tokio::time::sleep(Duration::from_secs(duration_s)).await;
        stop_notify.notify_waiters();
    }

    let mut n_writers_finished = 0;
    let mut n_readers_finished = 0;
    let mut n_errors = 0;
    let mut qps_sum: QPS = 0.0;

    while let Some((wtype, wid, res)) = finished_rx.recv().await {
        if let Err(e) = res.as_ref() {
            tracing::error!("{e}");
            n_errors += 1;
        }

        match wtype {
            WorkerType::Update => {
                n_writers_finished += 1;

                if n_writers_finished == num_update_workers {
                    let end_time = tokio::time::Instant::now();

                    tracing::info!(
                        "All updates completed in {}ms, {n_errors} workers encountered errors",
                        end_time.duration_since(start_time).as_millis()
                    );
                    stop_notify.notify_waiters();
                }
            },
            WorkerType::Reader => {
                n_readers_finished += 1;

                if let Ok(query_qps) = res {
                    let reader_avgqps: AvgQPS = query_qps.values().sum::<QPS>() / query_qps.len() as f64;
                    tracing::info!("Reader {} achieved {reader_avgqps:.2} AvgQPS", wid + 1);
                    qps_sum += reader_avgqps;

                    if let SubCommand::Stress { output_per_query_qps_csv: true, .. } = &opts.sub {
                        let mut w = csv::Writer::from_writer(std::io::stdout());

                        for (query_id, qps) in query_qps {
                            w.serialize(Datapoint { reader: wid, query_id, qps })?;
                        }
                    }
                }
            },
        }

        if n_writers_finished + n_readers_finished == num_update_workers + num_random_read_workers {
            break;
        }
    }

    tracing::info!(
        "The readers achieved {} AvgQPS",
        qps_sum / num_random_read_workers as f64,
    );

    anyhow::ensure!(n_errors == 0, "Test failed");
    Ok(())
}
