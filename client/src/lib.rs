//! `novelnote_client` provides [`Client`] for communicating with the HTTP API of NovelNote, a
//! self-hosted book tracker.

use std::{
    fmt::{self, Display, Formatter},
    sync::Arc,
};

use mime::Mime;
pub use reqwest::{self, Url};
use reqwest::{
    Response, StatusCode,
    header::{CONTENT_TYPE, HeaderValue},
};
use thiserror::Error;

/// NovelNote HTTP API client.
///
/// Cheap to clone as [`Arc`]s are used internally.
#[derive(Debug, Clone)]
pub struct Client {
    /// Internal [`reqwest`] client.
    client: reqwest::Client,

    /// Base URL of the NovelNote server to communicate with.
    base_url: Arc<Url>,
}

impl Client {
    /// Construct a [`Client`] from an existing [`reqwest::Client`].
    ///
    /// `base_url` should be the location of the NovelNote server, e.g.
    /// `https://novelnote.example.com`.
    #[must_use]
    pub fn new(client: reqwest::Client, base_url: Url) -> Self {
        Self {
            client,
            base_url: Arc::new(base_url),
        }
    }

    /// Check if the server is healthy and responding.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent or the server responds with an error or
    /// unexpected response.
    pub async fn health_check(&self) -> Result<(), HealthCheckError> {
        let response = self.get("api/health-check").await?;

        match response.status() {
            StatusCode::OK => Ok(()),
            StatusCode::INTERNAL_SERVER_ERROR => Err(HealthCheckError::InternalServerError),
            _ => Err(UnexpectedError::new(response).await.into()),
        }
    }

    /// Send a `GET` request to the `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    ///
    /// # Panics
    ///
    /// Panics if the `path` is not a valid URL when joined with the `base_url`.
    async fn get(&self, path: &str) -> Result<Response, reqwest::Error> {
        self.client.get(self.url(path)).send().await
    }

    /// Create a [`Url`] by joining the `path` to the `base_url` of the server.
    ///
    /// # Panics
    ///
    /// Panics if the `path` is not a valid URL when joined with the `base_url`.
    fn url(&self, path: &str) -> Url {
        self.base_url.join(path).expect("valid URL")
    }
}

/// Error returned when [`Client::health_check()`] fails.
#[derive(Error, Debug)]
pub enum HealthCheckError {
    /// Error sending request.
    #[error("error sending request")]
    Send(#[from] reqwest::Error),

    /// Server responded with `500 Internal Server Error`, server restart recommended.
    #[error("a server component is having issues, server restart recommended")]
    InternalServerError,

    /// Unexpected response from the server.
    #[error("unexpected response from the server")]
    Unexpected(#[from] UnexpectedError),
}

/// Error for when a [`Client`] makes a request, and the server returns an unexpected response.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub struct UnexpectedError {
    /// Status code of the response.
    pub status: StatusCode,

    /// Body of the response, if it contained one.
    pub message: Option<String>,
}

impl UnexpectedError {
    /// Create a [`UnexpectedError`], consuming a [`Response`].
    ///
    /// The error message is retrieved from the `response` if its `Content-Type` is `text/plain`.
    ///
    /// The `message` is [`None`] if the `Content-Type` header is not set, is an invalid value, or
    /// there is an  error decoding the response body.
    async fn new(response: Response) -> Self {
        let status = response.status();

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .map(HeaderValue::to_str)
            .and_then(Result::ok)
            .map(str::parse::<Mime>)
            .and_then(Result::ok);

        let message = if let Some(content_type) = content_type
            && content_type.essence_str() == "text/plain"
        {
            response.text().await.ok()
        } else {
            None
        };

        Self { status, message }
    }
}

impl Display for UnexpectedError {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let Self { status, message } = self;

        write!(f, "server responded with an unexpected status `{status}`")?;

        if let Some(message) = message.as_ref() {
            write!(f, " with message \"{message}\"")?;
        }

        Ok(())
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::{
        error::Error,
        path::{Path, PathBuf},
    };

    use novelnote_database::Database;
    use novelnote_server::{BindAddress, Server};
    use tempfile::TempDir;
    use tokio::sync::oneshot;
    use tokio_util::sync::CancellationToken;

    use super::*;

    /// Bind a [`Server`] to a Unix domain socket at `path` in a temporary directory, connect a
    /// [`Client`] to it, and execute the closure.
    pub(crate) async fn with_client<P, F, E>(
        path: P,
        database: Database,
        f: F,
    ) -> Result<(), Box<dyn Error>>
    where
        P: AsRef<Path>,
        F: AsyncFnOnce(Client) -> Result<(), E>,
        E: Into<Box<dyn Error>>,
    {
        let temp_dir = TempDir::new()?;
        let path = temp_dir.path().join(path).with_extension("socket");

        let server = Server {
            bind_address: BindAddress::Unix(path.clone()),
            asset_directory: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../target/dist")),
            database,
        };

        let (ready_sender, ready_receiver) = oneshot::channel();
        let cancellation_token = CancellationToken::new();
        let server = tokio::spawn(server.run(
            ready_sender,
            cancellation_token.child_token().cancelled_owned(),
        ));

        let client = reqwest::Client::builder().unix_socket(path).build()?;
        let client = Client::new(client, "http://example.com".parse()?);
        ready_receiver.await?;
        f(client).await.map_err(Into::into)?;

        cancellation_token.cancel();
        server.await??;
        temp_dir.close()?;

        Ok(())
    }

    #[tokio::test]
    async fn health_check() -> Result<(), Box<dyn Error>> {
        with_client(
            "test_health_check",
            Database::open_in_memory(1).await?,
            async |client| client.health_check().await,
        )
        .await
    }
}
