use crate::error::WorkerError;
use std::{
    ffi::{OsStr, OsString},
    io,
    sync::Arc,
    time::Duration,
};
use tokio::{process::Command, sync::Notify};

pub struct KillWorker {
    kill_script: OsString,
    restart_script: OsString,
    kill_delay: Duration,
}

impl KillWorker {
    pub fn new<OS: AsRef<OsStr>>(kill_script: OS, restart_script: OS, kill_delay: Duration) -> Self {
        Self {
            kill_script: kill_script.as_ref().to_owned(),
            restart_script: restart_script.as_ref().to_owned(),
            kill_delay,
        }
    }

    async fn run_command(script: &OsStr, map_err: impl Fn(io::Error) -> WorkerError) -> Result<(), WorkerError> {
        let mut child = Command::new("sh").arg("-c").arg(script).spawn().map_err(&map_err)?;

        let status = child.wait().await.map_err(&map_err)?;
        if status.success() {
            Ok(())
        } else {
            Err(map_err(io::Error::other("Exit code != 0")))
        }
    }

    async fn kill(&self) -> Result<(), WorkerError> {
        Self::run_command(&self.kill_script, WorkerError::KillFailed).await
    }

    async fn restart(&self) -> Result<(), WorkerError> {
        Self::run_command(&self.restart_script, WorkerError::RestartFailed).await
    }

    pub async fn execute(&mut self, stop: Arc<Notify>) -> Result<(), WorkerError> {
        let worker = async {
            loop {
                tokio::time::sleep(self.kill_delay).await;
                self.kill().await?;
                self.restart().await?;
            }
        };

        tokio::select! {
            res = worker => res,
            _ = stop.notified() => Ok(())
        }
    }
}
