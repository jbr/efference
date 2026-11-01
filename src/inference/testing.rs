//! Scratch space for tests that need a real filesystem, the scripted upstream tests
//! stand in for a provider with, and an endpoint for requests that go nowhere.

use crate::inference::api::Endpoint;
use crate::record::log::Provider;
use std::io;
use tempfile::TempDir;
use trillium_client::{Connector, Url};
use trillium_tokio::ClientConfig;

pub(crate) mod upstream;

/// A directory of its own under the OS temp dir, named `efference-<label>-` and a random
/// suffix, that is removed when the returned guard drops, a panicking test's included.
/// The directory is created before the name is returned and a taken name is retried, so
/// two tests never hold the same one. The guard has to live as long as anything the test
/// opens inside it, such as a database.
pub(crate) fn temp_dir(label: &str) -> TempDir {
    TempDir::with_prefix(format!("efference-{label}-")).expect("a scratch directory")
}

/// An endpoint at which nothing listens, reached through no socket: a request to it is
/// refused in process, as a port nothing listens on refuses it, and the transport takes
/// that as it takes any refused connection — it retries on its schedule and then fails
/// the request without a status. For a loop a test may wake to send something the test
/// does not read the outcome of: nothing is opened for a paused clock to race the
/// kernel's answer to.
pub(crate) fn refusing_endpoint(provider: Provider, path: &str) -> Endpoint {
    Endpoint::at(provider, format!("http://127.0.0.1:9{path}")).with_http(
        crate::platform::http::client_over(Refusing(ClientConfig::default())),
    )
}

/// The tokio connector, with the connection itself refused.
struct Refusing(ClientConfig);

impl Connector for Refusing {
    type Runtime = <ClientConfig as Connector>::Runtime;
    type Transport = <ClientConfig as Connector>::Transport;
    type Udp = <ClientConfig as Connector>::Udp;

    async fn connect(&self, _: &Url) -> io::Result<Self::Transport> {
        Err(io::ErrorKind::ConnectionRefused.into())
    }

    async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<std::net::SocketAddr>> {
        self.0.resolve(host, port).await
    }

    fn runtime(&self) -> Self::Runtime {
        self.0.runtime()
    }
}
