use std::{
    fmt::{Display, Formatter},
    io,
};
use thiserror::Error;

#[derive(Debug)]
pub struct InvalidStateVerboseInfo {
    pub query: String,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug)]
pub struct UpdateFailedVerboseInfo {
    pub query: String,
}

#[derive(Debug, Error)]
pub enum WorkerError {
    InvalidState {
        update_id: usize,
        verbose_info: Option<InvalidStateVerboseInfo>,
    },
    ReadFailed {
        query: String,
        err: reqwest::Error,
    },
    UpdateVerifyFailed {
        update_id: usize,
        err: reqwest::Error,
    },
    UpdateFailed {
        update_id: usize,
        err: reqwest::Error,
        verbose_info: Option<UpdateFailedVerboseInfo>,
    },
    KillFailed(io::Error),
    RestartFailed(io::Error),
}

impl Display for WorkerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerError::InvalidState { update_id, verbose_info } => {
                write!(f, "Unexpected result at update {update_id}")?;

                if let Some(InvalidStateVerboseInfo { query, expected, actual }) = verbose_info {
                    /*write!(f, "\nQuery: ")?;
                    if update.starts_with("INSERT DATA") || update.starts_with("DELETE DATA") {
                        format_insert_or_delete_data(f, update)?;
                    } else {
                        writeln!(f, "{}", update)?;
                    }*/

                    if expected != actual {
                        writeln!(
                            f,
                            "\nQuery:\n{}\n\nDifference between expected and actual state:\n{}",
                            query,
                            prettydiff::diff_lines(expected, actual)
                        )?;
                    }

                    Ok(())
                } else {
                    Ok(())
                }
            },
            WorkerError::UpdateVerifyFailed { update_id, err } => {
                write!(
                    f,
                    "Unable to execute verification query for update {update_id}. Error: {err}"
                )
            },
            WorkerError::UpdateFailed { update_id, err, verbose_info } => {
                write!(f, "Unable to execute update {update_id}. Error: {err}")?;

                if let Some(UpdateFailedVerboseInfo { query }) = verbose_info {
                    write!(f, "\nQuery: {query}")
                } else {
                    Ok(())
                }
            },
            WorkerError::ReadFailed { query, err } => {
                writeln!(
                    f,
                    "A reader was unable to execute a query. Error: {err}\nQuery: {query}"
                )
            },
            WorkerError::KillFailed(err) => write!(f, "Unable to kill server. Error: {err}"),
            WorkerError::RestartFailed(err) => write!(f, "Unable to restart server. Error: {err}"),
        }
    }
}
