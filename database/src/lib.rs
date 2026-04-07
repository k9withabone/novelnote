//! `novelnote_database` provides the SQLite [`Database`] interface for NovelNote, a self-hosted
//! book tracker.

use std::{
    any::Any,
    path::Path,
    sync::Arc,
    thread::{self, JoinHandle},
};

use thiserror::Error;
use tokio::{
    sync::{
        mpsc,
        oneshot::{self, error::TryRecvError},
    },
    task::spawn_blocking,
};
use tracing::{debug, instrument, trace, trace_span};
use tracing_error::SpanTrace;

/// Handle to NovelNote's database connection.
///
/// Uses [`Arc`]s internally so it is cheap to clone.
#[derive(Debug, Clone)]
pub struct Database {
    /// Channel for sending functions to call with the [`rusqlite::Connection`].
    sender: mpsc::Sender<CallFn>,

    /// Handle for gracefully closing the database connection.
    close_handle: Arc<CloseHandle>,
}

/// Boxed function for sending to the thread created in [`Connection::spawn()`].
type CallFn = Box<dyn FnOnce(&mut rusqlite::Connection) + Send>;

impl Database {
    /// Open a SQLite database at the given `path`.
    ///
    /// The database is opened in WAL mode.
    ///
    /// The connection to the database is spawned on a new thread. The returned handle can be used
    /// to communicate via a channel. `buffer` determines how many messages that channel will accept
    /// before exerting backpressure. 100 is a good default for `buffer`.
    ///
    /// # Errors
    ///
    /// Returns an error if there was a problem opening the database or initializing it.
    #[instrument(level = "debug", fields(path = %path.as_ref().display()))]
    pub async fn open<P>(path: P, buffer: usize) -> Result<Self, OpenError>
    where
        P: AsRef<Path> + Send + 'static,
    {
        let connection = Connection::open(path).await?;
        trace!("database connection open");

        let (sender, close_handle) = connection.spawn(buffer);

        Ok(Self {
            sender,
            close_handle: Arc::new(close_handle),
        })
    }

    /// Open a SQLite database in memory.
    ///
    /// The connection to the database is spawned on a new thread. The returned handle can be used
    /// to communicate via a channel. `buffer` determines how many messages that channel will accept
    /// before exerting backpressure. 100 is a good default for `buffer`.
    ///
    /// # Errors
    ///
    /// Returns an error if there was a problem opening the database or initializing it.
    #[instrument(level = "debug")]
    pub async fn open_in_memory(buffer: usize) -> Result<Self, OpenError> {
        let connection = Connection::open_in_memory().await?;
        trace!("database connection open");

        let (sender, close_handle) = connection.spawn(buffer);

        Ok(Self {
            sender,
            close_handle: Arc::new(close_handle),
        })
    }

    /// Check if the channel to the connection thread is open.
    #[must_use]
    #[inline]
    pub fn is_open(&self) -> bool {
        !self.sender.is_closed()
    }

    /// Close the database connection.
    ///
    /// # Errors
    ///
    /// Returns an error if another database connection handle still exists, there was a database
    /// error, or the connection thread panicked.
    #[instrument(level = "debug", skip(self))]
    pub async fn close(self) -> Result<(), CloseError> {
        let close_handle = Arc::into_inner(self.close_handle).ok_or(CloseError::OpenConnection)?;
        debug!("closing database connection");
        close_handle.close_and_join().await
    }
}

/// Error returned when [opening](Database::open()) a [`Database`] fails.
#[derive(Error, Debug)]
pub enum OpenError {
    /// Error opening the database for writing.
    #[error("error opening the database")]
    Open(#[from] DatabaseError),

    /// Error initializing the database.
    #[error("error initializing the database")]
    Init(#[from] InitError),

    /// The thread opening and initializing the database panicked.
    #[error(
        "panic while opening or initializing the database{}",
        .message
            .as_ref()
            .map_or_else(String::new, |message| format!(" with message \"{message}\""))
    )]
    Panic {
        /// Message the thread panicked with.
        message: Option<String>,

        /// Tracing context.
        context: SpanTrace,
    },
}

impl OpenError {
    /// Create a [`OpenError::Panic`], capturing the current span trace.
    fn from_panic(payload: Box<dyn Any + Send>) -> Self {
        Self::Panic {
            message: panic_payload_into_string(payload),
            context: SpanTrace::capture(),
        }
    }
}

/// SQLite error.
#[derive(Error, Debug)]
#[error("SQLite error")]
pub struct DatabaseError {
    /// Source of the error.
    source: rusqlite::Error,

    /// Tracing context.
    context: SpanTrace,
}

impl DatabaseError {
    /// Create a new [`DatabaseError`], capturing the current span trace.
    fn new(source: rusqlite::Error) -> Self {
        Self {
            source,
            context: SpanTrace::capture(),
        }
    }
}

/// Error returned when initializing the database fails.
#[derive(Error, Debug)]
#[error("error setting pragma options")]
pub struct InitError(#[from] pub DatabaseError);

/// Error returned when [closing](Database::close()) the [`Database`] connection fails.
#[derive(Error, Debug)]
pub enum CloseError {
    /// At least one other connection handle to the database still exists.
    #[error("other connection handles still exist")]
    OpenConnection,

    /// The database returned an error while closing.
    #[error("database error while closing")]
    Database(#[from] DatabaseError),

    /// The connection thread panicked before closing.
    #[error(
        "the connection thread panicked{}",
        .message
            .as_ref()
            .map_or_else(String::new, |message| format!(" with message \"{message}\""))
    )]
    Panic {
        /// Message the thread panicked with.
        message: Option<String>,

        /// Tracing context.
        context: SpanTrace,
    },
}

impl CloseError {
    /// Create a [`CloseError::Panic`], capturing the current span trace.
    fn from_panic(payload: Box<dyn Any + Send + 'static>) -> Self {
        Self::Panic {
            message: panic_payload_into_string(payload),
            context: SpanTrace::capture(),
        }
    }
}

/// Convert a payload from a caught panic into a string if that was the payload, which it usually
/// is.
fn panic_payload_into_string(payload: Box<dyn Any + Send>) -> Option<String> {
    payload.downcast().map_or_else(
        |payload| {
            payload
                .downcast::<&'static str>()
                .ok()
                .map(|string| (*string).to_owned())
        },
        |string| Some(*string),
    )
}

/// Connection to the SQLite database.
///
/// Wrapper around a [`rusqlite::Connection`] which is [spawned](Self::spawn()) onto another thread
/// and handles incoming task requests until the channel is closed.
#[derive(Debug)]
struct Connection(rusqlite::Connection);

impl Connection {
    /// Open a SQLite database at the given `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if there was a problem opening the database or initializing it.
    async fn open(path: impl AsRef<Path> + Send + 'static) -> Result<Self, OpenError> {
        let connection = spawn_blocking(|| {
            let connection = rusqlite::Connection::open(path).map_err(DatabaseError::new)?;
            init_db(&connection)?;
            Ok(connection)
        })
        .await
        .map_err(|error| OpenError::from_panic(error.into_panic()))
        .flatten()?;

        Ok(Self(connection))
    }

    /// Open a SQLite database in memory.
    ///
    /// # Errors
    ///
    /// Returns an error if there was a problem opening the database or initializing it.
    async fn open_in_memory() -> Result<Self, OpenError> {
        let connection = spawn_blocking(|| {
            let connection = rusqlite::Connection::open_in_memory().map_err(DatabaseError::new)?;
            init_db(&connection)?;
            Ok(connection)
        })
        .await
        .map_err(|error| OpenError::from_panic(error.into_panic()))
        .flatten()?;

        Ok(Self(connection))
    }

    /// Spawn a new thread, moving the connection to it.
    ///
    /// Returns the sender half of a channel for communicating with the connection and a handle for
    /// gracefully closing the connection.
    ///
    /// On close, the database is optimized.
    #[instrument(level = "trace")]
    fn spawn(self, buffer: usize) -> (mpsc::Sender<CallFn>, CloseHandle) {
        let Self(mut connection) = self;

        let (queue_sender, mut queue) = mpsc::channel::<CallFn>(buffer);
        let (close_sender, mut close_receiver) = oneshot::channel();

        let span = trace_span!("database_connection_thread").or_current();
        let join_handle = thread::spawn(move || {
            let _entered = span.entered();

            while let Some(f) = queue.blocking_recv() {
                f(&mut connection);
                if let Ok(()) | Err(TryRecvError::Closed) = close_receiver.try_recv() {
                    queue.close();
                }
            }

            connection.pragma_update(None, "analysis_limit", 400_i32)?;
            connection.execute("PRAGMA optimize", ())?;
            connection.close().map_err(|(_, error)| error)
        });

        (
            queue_sender,
            CloseHandle {
                close_sender,
                join_handle,
            },
        )
    }
}

/// Initialize the database, setting PRAGMAs.
///
/// # Errors
///
/// Returns an error if a PRAGMA setting cannot be set.
#[instrument(level = "debug", skip(connection))]
fn init_db(connection: &rusqlite::Connection) -> Result<(), InitError> {
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(DatabaseError::new)?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(DatabaseError::new)?;
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .map_err(DatabaseError::new)?;

    // TODO: migrations

    connection
        .execute("PRAGMA foreign_key_check", ())
        .map_err(DatabaseError::new)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(DatabaseError::new)?;

    Ok(())
}

/// Handle for gracefully closing the [`rusqlite::Connection`].
#[derive(Debug)]
struct CloseHandle {
    /// Channel for signaling that the connection should stop receiving new tasks and close.
    close_sender: oneshot::Sender<()>,

    /// Handle to the thread the connection is in.
    join_handle: JoinHandle<Result<(), rusqlite::Error>>,
}

impl CloseHandle {
    /// Signal to the connection thread that it should be closed and wait for it to do so.
    ///
    /// No new tasks will be added to the queue, existing tasks will be executed until none remain.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection thread has panicked or there was a database error while
    /// the connection was closed.
    #[instrument(level = "trace", skip(self))]
    async fn close_and_join(self) -> Result<(), CloseError> {
        let Self {
            close_sender,
            join_handle,
        } = self;

        #[expect(
            clippy::let_underscore_must_use,
            reason = "receiver only dropped if the thread has panicked"
        )]
        let _ = close_sender.send(());

        spawn_blocking(|| join_handle.join().map_err(CloseError::from_panic))
            .await
            .map_err(|error| CloseError::from_panic(error.into_panic()))??
            .map_err(|error| DatabaseError::new(error).into())
    }
}
