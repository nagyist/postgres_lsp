//! In here, there are the operations that run via standard input
//!
use crate::execute::Execution;
use crate::{CliDiagnostic, CliSession};
use pgt_console::{ConsoleExt, markup};
use pgt_fs::PgTPath;

pub(crate) fn run<'a>(
    session: CliSession,
    mode: &'a Execution,
    pgt_path: PgTPath,
    content: &'a str,
    verbose: bool,
) -> Result<(), CliDiagnostic> {
    let workspace = &*session.app.workspace;
    let console = &mut *session.app.console;
    let version = 0;

    console.append(markup! {{content}});
    Ok(())
}
