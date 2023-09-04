mod random_read_worker;
mod update_worker;

use clap::Parser;
use random_read_worker::RandomReadWorker;
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

#[derive(Parser)]
struct Opts {
    #[clap(short = 'r', long)]
    num_random_readers: usize,

    #[clap(short = 'w', long)]
    num_update_workers: usize,

    #[clap(short = 'q', long)]
    query_dir: PathBuf,

    #[clap(long)]
    random_reader_period_ms: u64,

    query_endpoint: Url,
    update_endpoint: Url,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts: Opts = Opts::parse();

    tracing_subscriber::fmt::init();

    let (update_workers, random_read_workers) = {
        let mut update_workers = Vec::with_capacity(opts.num_update_workers);
        for worker in 1..=opts.num_update_workers {
            let mut w = UpdateWorker::new(
                Path::new(&format!("rdf/worker_{worker}")),
                opts.query_endpoint.clone(),
                opts.update_endpoint.clone(),
            )?;

            w.prepare().await?;
            update_workers.push(w);
        }

        let mut random_read_workers = Vec::with_capacity(opts.num_random_readers);
        let query = std::fs::read_to_string(opts.query_dir.join("random_read.sparql"))?;

        for _ in 0..opts.num_random_readers {
            let w = RandomReadWorker::new(
                query.clone(),
                Duration::from_millis(opts.random_reader_period_ms),
                opts.query_endpoint.clone(),
            );
            random_read_workers.push(w);
        }

        (update_workers, random_read_workers)
    };

    let start_barrier = Arc::new(Barrier::new(opts.num_update_workers + opts.num_random_readers));
    let (finished_tx, mut finished_rx) = tokio::sync::mpsc::channel(opts.num_update_workers);

    for (worker_id, update_worker) in update_workers.into_iter().enumerate() {
        let start_barrier = start_barrier.clone();
        let finished_tx = finished_tx.clone();

        tokio::spawn(async move {
            start_barrier.wait().await;
            tracing::info!("Starting update worker {worker_id}");

            if let Err(e) = update_worker.execute().await {
                tracing::error!("{e}");
            }

            finished_tx.send(()).await.unwrap();
        });
    }

    let stop_notify = Arc::new(Notify::new());

    for (worker_id, rr_worker) in random_read_workers.into_iter().enumerate() {
        let start_barrier = start_barrier.clone();
        let stop_notify = stop_notify.clone();

        tokio::spawn(async move {
            start_barrier.wait().await;
            tracing::info!("Starting random read worker {worker_id}");

            if let Err(e) = rr_worker.execute(stop_notify).await {
                tracing::error!("{e}");
            }
        });
    }

    let mut n_finished = 0;
    while let Some(_) = finished_rx.recv().await {
        n_finished += 1;

        if n_finished == opts.num_update_workers {
            tracing::info!("All updates completed, shutting down");
            stop_notify.notify_waiters();
            break;
        }
    }

    Ok(())
}
