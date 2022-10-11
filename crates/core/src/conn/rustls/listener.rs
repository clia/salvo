//! rustls module
use std::io::{Error as IoError, ErrorKind, Result as IoResult};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::stream::BoxStream;
use futures_util::task::noop_waker_ref;
use futures_util::{Stream, StreamExt};
use tokio::net::ToSocketAddrs;
use tokio_rustls::server::TlsStream;

use crate::conn::{Accepted, Acceptor, IntoConfigStream, SocketAddr, TcpListener, TlsConnStream};
use crate::{async_trait, Listener};

use super::RustlsConfig;

/// RustlsListener

pub struct RustlsListener<C, T> {
    config_stream: C,
    inner: T,
}

impl<C, T> RustlsListener<C, T>
where
    C: IntoConfigStream<RustlsConfig>,
    T: Listener + Send,
{
    /// Create a new `RustlsListener`.
    #[inline]
    pub fn new(config_stream: C, inner: T) -> Self {
        RustlsListener { config_stream, inner }
    }
}

#[async_trait]
impl<C, T> Listener for RustlsListener<C, T>
where
    C: IntoConfigStream<RustlsConfig>,
    T: Listener + Send,
    T::Acceptor: Send + 'static,
{
    type Acceptor = RustlsAcceptor<BoxStream<'static, RustlsConfig>, T::Acceptor>;
    async fn into_acceptor(self) -> IoResult<Self::Acceptor> {
        Ok(RustlsAcceptor::new(
            self.config_stream.into_stream().boxed(),
            self.inner.into_acceptor().await?,
        ))
    }
}

impl<C, T> RustlsListener<C, TcpListener<T>>
where
    C: IntoConfigStream<RustlsConfig>,
    T: ToSocketAddrs + Send + 'static,
{
    /// Bind to socket address.
    #[inline]
    pub fn bind(config: C, addr: T) -> RustlsListener<C::Stream, TcpListener<T>> {
        RustlsListener {
            config_stream: config.into_stream(),
            inner: TcpListener::bind(addr),
        }
    }
}

/// RustlsAcceptor
pub struct RustlsAcceptor<C, T> {
    config_stream: C,
    inner: T,
    tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
}
impl<C, T> RustlsAcceptor<C, T> {
    /// Create a new `RustlsAcceptor`.
    pub fn new(config_stream: C, inner: T) -> RustlsAcceptor<C, T> {
        RustlsAcceptor {
            config_stream,
            inner,
            tls_acceptor: None,
        }
    }
}

#[async_trait]
impl<C, T> Acceptor for RustlsAcceptor<C, T>
where
    C: Stream<Item = RustlsConfig> + Send + Unpin + 'static,
    T: Acceptor + Send + 'static,
{
    type Conn = TlsConnStream<TlsStream<T::Conn>>;

    fn local_addrs(&self) -> Vec<&SocketAddr> {
        self.inner.local_addrs()
    }

    #[inline]
    async fn accept(&mut self) -> IoResult<Accepted<Self::Conn>> {
        let config = {
            let mut config = None;
            while let Poll::Ready(Some(item)) =
                Pin::new(&mut self.config_stream).poll_next(&mut Context::from_waker(noop_waker_ref()))
            {
                config = Some(item);
            }
            config
        };
        if let Some(config) = config {
            let tls_acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config.build_server_config()?));
            if self.tls_acceptor.is_some() {
                tracing::info!("tls config changed.");
            } else {
                tracing::info!("tls config loaded.");
            }
            self.tls_acceptor = Some(tls_acceptor);
        }
        let Accepted {
            stream,
            local_addr,
            remote_addr,
        } = self.inner.accept().await?;
        let tls_acceptor = match &self.tls_acceptor {
            Some(tls_acceptor) => tls_acceptor,
            None => return Err(IoError::new(ErrorKind::Other, "no valid tls config.")),
        };

        let stream = TlsConnStream::new(tls_acceptor.accept(stream));
        Ok(Accepted {
            stream,
            local_addr,
            remote_addr,
        })
    }
}
