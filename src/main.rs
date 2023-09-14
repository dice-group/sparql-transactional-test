mod random_read_worker;
mod update_worker;

use clap::Parser;
use random_read_worker::{RandomLimitSelectStartQueryGenerator, RandomReadWorker};
use reqwest::Url;
use std::{
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

#[derive(Parser)]
enum Opts {
    Stress {
        #[clap(short = 'r', long)]
        num_random_readers: usize,

        #[clap(short = 't', long)]
        duration_s: u64,

        query_endpoint: Url,
    },
    Verify {
        #[clap(short = 'r', long)]
        num_random_read_workers: usize,

        #[clap(short = 'w', long)]
        num_update_workers: usize,

        #[clap(short = 't', long, default_value_t = 0)]
        runtime_s: u64,

        #[clap(short = 'q', long)]
        query_dir: PathBuf,

        query_endpoint: Url,
        update_endpoint: Url,
    },
}

enum WorkerType {
    Update,
    Reader,
}

fn make_random_readers(query_endpoint: &Url, num_random_readers: usize) -> Vec<RandomReadWorker> {
    let mut random_read_workers = Vec::with_capacity(num_random_readers);

    for _ in 0..num_random_readers {
        let w = RandomReadWorker::new(
            Box::new(RandomLimitSelectStartQueryGenerator),
            query_endpoint.clone(),
        );
        random_read_workers.push(w);
    }

    random_read_workers
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
    let opts: Opts = Opts::parse();

    tracing_subscriber::fmt::init();

    let (update_workers, random_read_workers) = match opts {
        Opts::Stress { num_random_readers, duration_s: _, ref query_endpoint } => {
            (vec![], make_random_readers(query_endpoint, num_random_readers))
        },
        Opts::Verify {
            num_random_read_workers,
            num_update_workers,
            runtime_s: _,
            ref query_dir,
            ref query_endpoint,
            ref update_endpoint,
        } => (
            make_update_workers(query_endpoint, update_endpoint, num_update_workers, query_dir)?,
            make_random_readers(query_endpoint, num_random_read_workers),
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
                .send((WorkerType::Update, worker_id, res.map(|_| 0.0)))
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
    let start_time = std::time::Instant::now();

    if let Opts::Stress { duration_s, .. } = opts {
        tokio::time::sleep(Duration::from_secs(duration_s)).await;
        stop_notify.notify_waiters();
    }

    let mut n_writers_finished = 0;
    let mut n_readers_finished = 0;
    let mut n_errors = 0;
    let mut qps_sum = 0.0;

    while let Some((wtype, wid, res)) = finished_rx.recv().await {
        if let Err(e) = res.as_ref() {
            tracing::error!("{e}");
            n_errors += 1;
        }

        match wtype {
            WorkerType::Update => {
                n_writers_finished += 1;

                if n_writers_finished == num_update_workers {
                    let end_time = std::time::Instant::now();

                    tracing::info!(
                        "All updates completed in {}ms, {n_errors} workers encountered errors",
                        end_time.duration_since(start_time).as_millis()
                    );
                    stop_notify.notify_waiters();
                }
            },
            WorkerType::Reader => {
                n_readers_finished += 1;

                if let Ok(qps) = res {
                    tracing::info!("Reader {} achieved {qps} QPS", wid + 1);
                    qps_sum += qps;
                }
            },
        }

        if n_writers_finished + n_readers_finished == num_update_workers + num_random_read_workers {
            break;
        }
    }

    tracing::info!(
        "The readers achieved {} AvgQPS",
        qps_sum / num_random_read_workers as f64
    );

    Ok(())
}
