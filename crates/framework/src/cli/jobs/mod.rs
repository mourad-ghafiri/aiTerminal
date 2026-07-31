//! `@job` — a task the terminal runs for you, now or on a schedule, in the foreground
//! or detached. Split by what each piece does: reading the grammar (`args`), turning a
//! request into a record (`create`), the `in`/`at`/`every` phrases (`schedule`), the
//! detached spawn (`spawn`), running a shell job under the guard (`shell`), and the
//! listings (`show`).

pub(crate) mod args;
pub(crate) mod create;
pub(crate) mod schedule;
pub(crate) mod shell;
pub(crate) mod show;
pub(crate) mod spawn;
