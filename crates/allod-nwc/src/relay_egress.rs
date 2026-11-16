//! Public-only WebSocket transport. DNS is resolved and filtered on every connection and
//! reconnect, then TCP uses that exact address while TLS and HTTP retain original hostname.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_wsocket::message::CloseFrame;
use async_wsocket::Message;
use futures_util::stream::SplitSink;
use futures_util::{Sink, StreamExt};
use nostr::types::Url;
use nostr_sdk::client::Client;
use nostr_sdk::error::Error;
use nostr_sdk::transport::websocket::{WebSocketSink, WebSocketStream, WebSocketTransport};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as NativeMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream as NativeWebSocketStream};

type NativeSocket = NativeWebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Clone, Copy)]
pub(crate) struct PublicWebSocketTransport {
    allow_private: bool,
}

impl PublicWebSocketTransport {
    pub(crate) fn new(allow_private: bool) -> Self {
        Self { allow_private }
    }
}

pub(crate) fn client(allow_private: bool) -> Client {
    Client::builder().websocket_transport(PublicWebSocketTransport::new(allow_private)).build()
}

impl WebSocketTransport for PublicWebSocketTransport {
    fn support_ping(&self) -> bool {
        true
    }

    fn connect<'a>(
        &'a self,
        url: &'a Url,
        proxy: Option<SocketAddr>,
    ) -> Pin<Box<dyn Future<Output = Result<(WebSocketSink, WebSocketStream), Error>> + Send + 'a>>
    {
        Box::pin(async move {
            if proxy.is_some() {
                return Err(Error::transport(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "relay proxy would bypass public-address enforcement",
                )));
            }
            let checked =
                allod_lnurl::check_egress_url(url.as_str(), "wss", Some("ws"), self.allow_private)
                    .map_err(|message| {
                        Error::transport(io::Error::new(io::ErrorKind::PermissionDenied, message))
                    })?;
            let host = checked.host_str().ok_or_else(|| {
                Error::transport(io::Error::new(io::ErrorKind::InvalidInput, "relay has no host"))
            })?;
            let port = checked.port_or_known_default().ok_or_else(|| {
                Error::transport(io::Error::new(io::ErrorKind::InvalidInput, "relay has no port"))
            })?;

            let connection = async {
                let addresses =
                    allod_lnurl::resolve_egress_addrs(host, port, self.allow_private).await?;
                let mut last_error = None;
                for address in addresses {
                    match TcpStream::connect(address).await {
                        Ok(stream) => {
                            let (socket, _) =
                                tokio_tungstenite::client_async_tls(checked.as_str(), stream)
                                    .await
                                    .map_err(io::Error::other)?;
                            return Ok::<NativeSocket, io::Error>(socket);
                        }
                        Err(error) => last_error = Some(error),
                    }
                }
                Err(last_error.unwrap_or_else(|| {
                    io::Error::new(io::ErrorKind::AddrNotAvailable, "no allowed relay address")
                }))
            };
            let socket = tokio::time::timeout(Duration::from_secs(10), connection)
                .await
                .map_err(|_| {
                    Error::transport(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "relay connection timed out",
                    ))
                })?
                .map_err(Error::transport)?;
            let (tx, rx) = socket.split();
            let sink: WebSocketSink = Box::pin(MessageSink(tx));
            let stream: WebSocketStream = Box::pin(rx.map(|result| {
                result
                    .map_err(Error::transport)
                    .and_then(|message| native_message(message).map_err(Error::transport))
            }));
            Ok((sink, stream))
        })
    }
}

fn native_message(message: NativeMessage) -> io::Result<Message> {
    Ok(match message {
        NativeMessage::Text(text) => Message::Text(text.to_string()),
        NativeMessage::Binary(data) => Message::Binary(data.to_vec()),
        NativeMessage::Ping(data) => Message::Ping(data.to_vec()),
        NativeMessage::Pong(data) => Message::Pong(data.to_vec()),
        NativeMessage::Close(frame) => {
            Message::Close(frame.map(|frame| CloseFrame {
                code: frame.code.into(),
                reason: frame.reason.to_string(),
            }))
        }
        NativeMessage::Frame(_) => {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "unexpected raw relay frame"))
        }
    })
}

struct MessageSink(SplitSink<NativeSocket, NativeMessage>);

impl Sink<Message> for MessageSink {
    type Error = Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0).poll_ready(cx).map_err(Error::transport)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        Pin::new(&mut self.0).start_send(item.into()).map_err(Error::transport)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0).poll_flush(cx).map_err(Error::transport)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0).poll_close(cx).map_err(Error::transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transport_refuses_private_and_cleartext_public_relays() {
        let transport = PublicWebSocketTransport::new(false);
        for relay in ["wss://127.0.0.1:7777", "wss://[::1]", "ws://example.com"] {
            let url = Url::parse(relay).unwrap();
            let error = match transport.connect(&url, None).await {
                Ok(_) => panic!("{relay} should be refused"),
                Err(error) => error.to_string(),
            };
            assert!(error.contains("public") || error.contains("must use wss"), "{relay}: {error}");
        }
    }
}
