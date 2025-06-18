mod error;
mod kill_worker;
mod random_read_worker;
mod update_worker;

use crate::{
    error::WorkerError,
    kill_worker::KillWorker,
    random_read_worker::{FileSourceQueryGenerator, QueryGenerator},
};
use anyhow::Context;
use clap::Parser;
use random_read_worker::{RandomLimitSelectStartQueryGenerator, RandomReadWorker};
use reqwest::Url;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::IsTerminal,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    select,
    sync::{Barrier, Notify},
};
use update_worker::UpdateWorker;

type Query = String;
type Qps = f64;
type AvgQps = f64;

#[derive(serde::Serialize)]
struct QPSMeasurement {
    reader: usize,
    query_id: usize,
    qps: f64,
}

struct UpdateJobResult {
    worker_id: usize,
    result: Result<(), WorkerError>,
}

struct ReadJobResult {
    worker_id: usize,
    qps_measurements: Result<BTreeMap<usize, Qps>, WorkerError>,
}

struct KillJobResult {
    result: Result<(), WorkerError>,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum WorkerBehaviour {
    IgnoreConnectionError,
    ReportConnectionError,
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
enum VerifySubcommand {
    Durability {
        /// If present, the stress test will use this script to start the server on test begin.
        ///
        /// This option must be used in conjunction with --kill-script and --restart-script.
        #[clap(long)]
        start_script: OsString,

        /// If present, the stress test will periodically
        /// kill (i.e. uncleanly shutdown) the server using this script.
        ///
        /// This option must be used in conjunction with --start-script and --restart-script.
        #[clap(long)]
        kill_script: OsString,

        /// If present, the stress test will restart the server
        /// after a kill using this script.
        ///
        /// This option must be used in conjunction with --start-script and --kill-script.
        #[clap(long)]
        restart_script: OsString,

        /// The number of seconds between server kills
        #[clap(long, default_value_t = 10)]
        kill_delay_s: u64,
    },
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

        /// URL to SPARQL Graph Store Protocol endpoint
        graph_store_endpoint: Url,

        /// If an error occurs, log the query string of the query that caused it.
        /// Warning the string can potentially be very long.
        #[clap(short = 'v', long)]
        verbose: bool,

        #[clap(subcommand)]
        sub: Option<VerifySubcommand>,
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

#[tokio::main]
async fn main() {
    let opts: Command = Command::parse();

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal() && !opts.no_color)
        .init();

    if let Err(e) = run(opts).await {
        tracing::error!("{e}");
        std::process::exit(1);
    }
}

async fn run(opts: Command) -> anyhow::Result<()> {
    let (update_workers, random_read_workers, kill_worker) = match &opts.sub {
        SubCommand::Stress { reader_opts, query_endpoint, .. } => (
            vec![],
            make_random_readers(query_endpoint, reader_opts, WorkerBehaviour::ReportConnectionError)?,
            None,
        ),
        SubCommand::Verify {
            reader_opts,
            num_update_workers,
            update_query_dir,
            query_endpoint,
            update_endpoint,
            graph_store_endpoint,
            verbose,
            sub,
        } => {
            let behav = if sub.is_none() {
                WorkerBehaviour::ReportConnectionError
            } else {
                WorkerBehaviour::IgnoreConnectionError
            };

            (
                make_update_workers(
                    query_endpoint,
                    update_endpoint,
                    graph_store_endpoint,
                    *num_update_workers,
                    update_query_dir,
                    *verbose,
                    behav,
                )?,
                make_random_readers(query_endpoint, reader_opts, behav)?,
                make_kill_worker(sub.as_ref()),
            )
        },
    };

    let num_update_workers = update_workers.len();
    let num_random_read_workers = random_read_workers.len();
    let num_kill_workers = kill_worker.is_some() as usize;

    let start_barrier = Arc::new(Barrier::new(
        num_update_workers + num_random_read_workers + num_kill_workers + 1,
    ));
    let (updates_finished_tx, mut updates_finished_rx) = tokio::sync::mpsc::channel(num_update_workers);

    for (update_worker, worker_id) in update_workers.into_iter().zip(1..) {
        let start_barrier = start_barrier.clone();
        let finished_tx = updates_finished_tx.clone();

        tokio::spawn(async move {
            start_barrier.wait().await;
            tracing::info!("Starting update worker {}", worker_id);

            let result = update_worker.execute().await;
            finished_tx.send(UpdateJobResult { worker_id, result }).await.unwrap();
        });
    }

    let stop_notify = Arc::new(Notify::new());
    let (readers_finished_tx, mut readers_finished_rx) = tokio::sync::mpsc::channel(num_random_read_workers);

    for (mut rr_worker, worker_id) in random_read_workers.into_iter().zip(1..) {
        let start_barrier = start_barrier.clone();
        let finished_tx = readers_finished_tx.clone();
        let stop_notify = stop_notify.clone();

        tokio::spawn(async move {
            start_barrier.wait().await;
            tracing::info!("Starting random read worker {worker_id}");

            let qps_measurements = rr_worker.execute(stop_notify).await;
            finished_tx
                .send(ReadJobResult { worker_id, qps_measurements })
                .await
                .unwrap();
        });
    }

    let (kill_worker_finished_tx, mut kill_worker_finished_rx) = tokio::sync::mpsc::channel(1);

    if let Some(mut kill_worker) = kill_worker {
        let start_barrier = start_barrier.clone();
        let finished_tx = kill_worker_finished_tx.clone();
        let stop_notify = stop_notify.clone();

        tokio::spawn(async move {
            start_barrier.wait().await;
            tracing::info!("Starting kill worker");

            let result = kill_worker.execute(stop_notify).await;
            finished_tx.send(KillJobResult { result }).await.unwrap();
        });
    }

    drop(updates_finished_tx);
    drop(readers_finished_tx);
    drop(kill_worker_finished_tx);

    if let SubCommand::Verify { sub: Some(VerifySubcommand::Durability { start_script, .. }), .. } = &opts.sub {
        match tokio::process::Command::new("sh")
            .arg("-c")
            .arg(start_script)
            .status()
            .await
        {
            Ok(status) if !status.success() => {
                tracing::error!("Starting server failed. Error: Script returned exit status != 0");
                return Err(anyhow::anyhow!("Test failed, unable to perform lifecycle management"));
            },
            Err(e) => {
                tracing::error!("Starting server failed. Error: {e}");
                return Err(anyhow::anyhow!("Test failed, unable to perform lifecycle management"));
            },
            _ => (),
        }
    }

    start_barrier.wait().await;
    let start_time = tokio::time::Instant::now();

    if let SubCommand::Stress { duration_s, .. } = opts.sub {
        tokio::time::sleep(Duration::from_secs(duration_s)).await;
        stop_notify.notify_waiters();
    }

    let mut n_update_errors = 0;

    loop {
        select! {
            ujobres = updates_finished_rx.recv() => {
                if let Some(UpdateJobResult { worker_id, result }) = ujobres {
                    if let Err(e) = result {
                        tracing::error!("Update worker {worker_id} encountered an error: {e}");
                        n_update_errors += 1;
                    }
                } else {
                    break;
                }
            },
            kjobres = kill_worker_finished_rx.recv() => {
                if let Some(KillJobResult { result: Err(e) }) = kjobres {
                    tracing::error!("Kill worker encountered an error: {e}");
                    return Err(anyhow::anyhow!("Test failed, unable to perform lifecycle management"));
                }
            },
        }
    }

    let end_time = tokio::time::Instant::now();
    tracing::info!(
        "All updates completed in {:.2}s, {n_update_errors} update workers encountered errors",
        end_time.duration_since(start_time).as_secs_f64()
    );

    stop_notify.notify_waiters();

    let mut qps_sum: Qps = 0.0;

    while let Some(ReadJobResult { worker_id, qps_measurements }) = readers_finished_rx.recv().await {
        match qps_measurements {
            Ok(qps_measurements) => {
                let reader_avgqps: AvgQps = qps_measurements.values().sum::<Qps>() / qps_measurements.len() as f64;
                tracing::info!("Random read worker {worker_id} achieved {reader_avgqps:.2} AvgQPS");
                qps_sum += reader_avgqps;

                if let SubCommand::Stress { output_per_query_qps_csv: true, .. } = &opts.sub {
                    let mut w = csv::Writer::from_writer(std::io::stdout());

                    for (query_id, qps) in qps_measurements {
                        w.serialize(QPSMeasurement { reader: worker_id, query_id, qps })?;
                    }
                }
            },
            Err(e) => {
                tracing::error!("Random read worker {worker_id} encountered an error: {e}");
            },
        }
    }

    tracing::info!(
        "The random read workers achieved {} AvgQPS",
        qps_sum / num_random_read_workers as f64,
    );

    if let SubCommand::Verify { sub: Some(VerifySubcommand::Durability { kill_script, .. }), .. } = &opts.sub {
        let _ = tokio::process::Command::new("sh").arg("-c").arg(kill_script).spawn();
    }

    if n_update_errors > 0 {
        Err(anyhow::anyhow!("Test failed, errors were encountered"))
    } else {
        Ok(())
    }
}

fn make_kill_worker(kill_opts: Option<&VerifySubcommand>) -> Option<KillWorker> {
    kill_opts.map(
        |VerifySubcommand::Durability { kill_script, restart_script, kill_delay_s, .. }| {
            KillWorker::new(kill_script, restart_script, Duration::from_secs(*kill_delay_s))
        },
    )
}

fn make_random_readers(
    query_endpoint: &Url,
    ReaderOpts { num_random_read_workers, random_read_workers_query_file }: &ReaderOpts,
    behav: WorkerBehaviour,
) -> anyhow::Result<Vec<RandomReadWorker>> {
    let mut random_read_workers = Vec::with_capacity(*num_random_read_workers);
    for _ in 0..*num_random_read_workers {
        let query_gen: Box<dyn QueryGenerator + Send> = if let Some(query_file) = &random_read_workers_query_file {
            Box::new(FileSourceQueryGenerator::new(query_file).context("Unable to open queries file")?)
        } else {
            Box::new(RandomLimitSelectStartQueryGenerator)
        };

        let w = RandomReadWorker::new(query_gen, query_endpoint.clone(), behav);
        random_read_workers.push(w);
    }

    Ok(random_read_workers)
}

fn make_update_workers(
    query_endpoint: &Url,
    update_endpoint: &Url,
    graph_store_endpoint: &Url,
    num_update_workers: usize,
    query_dir: &Path,
    verbose: bool,
    behav: WorkerBehaviour,
) -> anyhow::Result<Vec<UpdateWorker>> {
    let mut update_workers = Vec::with_capacity(num_update_workers);
    for worker in 0..num_update_workers {
        let w = UpdateWorker::new(
            Path::new(&query_dir.join(format!("worker_{worker}"))),
            query_endpoint.clone(),
            update_endpoint.clone(),
            graph_store_endpoint.clone(),
            verbose,
            behav,
        )?;

        update_workers.push(w);
    }

    Ok(update_workers)
}
