use std::{
    fmt::{Display, Formatter},
    io,
};
use thiserror::Error;

#[derive(Debug)]
pub struct InvalidStateVerboseInfo {
    pub expected: (String, String),
    pub actual: (String, String),
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
        subject: String,
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

fn format_insert_or_delete_data(f: &mut Formatter<'_>, q: &str) -> std::fmt::Result {
    let (head, body) = q.split_once("{ <").unwrap();
    let (body, _) = body.rsplit_once(". }").unwrap();

    writeln!(f, "{head}{{")?;

    for triple in body.split(". <") {
        writeln!(f, "    <{triple}.")?;
    }

    writeln!(f, "}}")?;
    Ok(())
}

impl Display for WorkerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerError::InvalidState { update_id, verbose_info } => {
                write!(f, "Unexpected result at update {update_id}")?;

                if let Some(InvalidStateVerboseInfo { expected, actual }) = verbose_info {
                    /*write!(f, "\nQuery: ")?;
                    if update.starts_with("INSERT DATA") || update.starts_with("DELETE DATA") {
                        format_insert_or_delete_data(f, update)?;
                    } else {
                        writeln!(f, "{}", update)?;
                    }*/

                    writeln!(f, "\nExpected Default :\n{}", actual.0)?;
                    writeln!(f, "\nActual Default:\n{}", expected.0)?;

                    write!(f, "\nDiff:\n{}", prettydiff::diff_lines(&expected.0, &actual.0))?;

                    writeln!(f, "\nExpected Default :\n{}", actual.1)?;
                    writeln!(f, "\nActual Default:\n{}", expected.1)?;

                    write!(f, "\nDiff:\n{}", prettydiff::diff_lines(&expected.1, &actual.1))?;

                    Ok(())
                } else {
                    Ok(())
                }
            },
            WorkerError::UpdateVerifyFailed { update_id, subject, err } => {
                write!(
                    f,
                    "Unable to execute verification query for update {update_id}. Error: {err}\nSubject: {subject}"
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
