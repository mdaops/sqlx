//! Experimental P3 backend. TCP, async timers and local tasks; no blocking thread pool.
use super::{JoinHandle, TimeoutError};
use crate::{io::ReadBuf, net::Socket};
use bytes::Bytes;
use futures_util::future::{select, Either};
use std::{
    future::Future,
    io,
    pin::Pin,
    sync::Mutex,
    task::{ready, Context, Poll},
    time::Duration,
};
use wasip3::sockets::{
    ip_name_lookup,
    types::{
        ErrorCode, IpAddress, IpAddressFamily, IpSocketAddress, Ipv4SocketAddress,
        Ipv6SocketAddress, TcpSocket,
    },
};
use wit_bindgen::rt::async_support::{FutureReader, StreamReader, StreamResult, StreamWriter};

pub async fn sleep(duration: Duration) {
    wasip3::clocks::monotonic_clock::wait_for(duration.as_nanos().min(u128::from(u64::MAX)) as u64)
        .await;
}

pub async fn timeout<F: Future>(duration: Duration, f: F) -> Result<F::Output, TimeoutError> {
    match select(Box::pin(f), Box::pin(sleep(duration))).await {
        Either::Left((value, _)) => Ok(value),
        Either::Right(_) => Err(TimeoutError),
    }
}

pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let (tx, rx) = futures_channel::oneshot::channel();
    wit_bindgen::spawn_local(async move {
        let _ = tx.send(future.await);
    });
    JoinHandle::WasiP3(rx)
}

type Done = FutureReader<Result<(), ErrorCode>>;
type Input = (StreamReader<u8>, Done);
type Output = (StreamWriter<u8>, Done);
type Task<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type ReadResult = (Option<Input>, io::Result<Vec<u8>>);
type WriteResult = (Option<Output>, io::Result<()>);

fn error(e: ErrorCode) -> io::Error {
    let kind = match e {
        ErrorCode::ConnectionRefused => io::ErrorKind::ConnectionRefused,
        ErrorCode::ConnectionReset | ErrorCode::ConnectionBroken | ErrorCode::ConnectionAborted => {
            io::ErrorKind::ConnectionReset
        }
        ErrorCode::Timeout => io::ErrorKind::TimedOut,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, e.to_string())
}
fn blocked() -> io::Error {
    io::ErrorKind::WouldBlock.into()
}

/// Mutex gives the Send future slots Sync without asserting unsafe traits.
/// SQLx calls Socket methods through &mut self; no lock is held across an await.
pub struct P3Socket {
    state: Mutex<State>,
}
struct State {
    input: Option<Input>,
    output: Option<Output>,
    reading: Option<Task<ReadResult>>,
    writing: Option<Task<WriteResult>>,
    shutdown: Option<Task<io::Result<()>>>,
    buffer: Bytes,
    read_error: Option<io::Error>,
    eof: bool,
    // Streams and their pending operations drop before the socket resource.
    _socket: TcpSocket,
}

impl P3Socket {
    pub async fn connect(host: &str, port: u16) -> io::Result<Self> {
        let host = host.trim_matches(&['[', ']'][..]);
        let addresses = match host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(ip)) => {
                let [a, b, c, d] = ip.octets();
                vec![IpAddress::Ipv4((a, b, c, d))]
            }
            Ok(std::net::IpAddr::V6(ip)) => {
                let [a, b, c, d, e, f, g, h] = ip.segments();
                vec![IpAddress::Ipv6((a, b, c, d, e, f, g, h))]
            }
            Err(_) => ip_name_lookup::resolve_addresses(host.to_string())
                .await
                .map_err(|e| io::Error::other(format!("DNS {host}: {e}")))?,
        };
        let mut last_error = io::Error::new(io::ErrorKind::NotFound, "no resolved addresses");
        for address in addresses {
            let (family, address) = match address {
                IpAddress::Ipv4(address) => (
                    IpAddressFamily::Ipv4,
                    IpSocketAddress::Ipv4(Ipv4SocketAddress { address, port }),
                ),
                IpAddress::Ipv6(address) => (
                    IpAddressFamily::Ipv6,
                    IpSocketAddress::Ipv6(Ipv6SocketAddress {
                        address,
                        port,
                        flow_info: 0,
                        scope_id: 0,
                    }),
                ),
            };
            let socket = TcpSocket::create(family)
                .map_err(|e| io::Error::other(format!("TCP create: {e}")))?;
            let target = format!("{address:?}");
            if let Err(e) = socket.connect(address).await {
                let cause = error(e);
                last_error = io::Error::new(cause.kind(), format!("TCP connect {target}: {cause}"));
                continue;
            }
            let input = socket.receive();
            let (writer, reader) = wasip3::wit_stream::new::<u8>();
            let output = (writer, socket.send(reader));
            return Ok(Self {
                state: Mutex::new(State {
                    input: Some(input),
                    output: Some(output),
                    reading: None,
                    writing: None,
                    shutdown: None,
                    buffer: Bytes::new(),
                    read_error: None,
                    eof: false,
                    _socket: socket,
                }),
            });
        }
        Err(last_error)
    }
}

async fn read_chunk((mut reader, done): Input) -> ReadResult {
    loop {
        let (status, data) = reader.read(Vec::with_capacity(16 * 1024)).await;
        match status {
            StreamResult::Complete(_) if data.is_empty() => continue,
            StreamResult::Complete(_) => return (Some((reader, done)), Ok(data)),
            StreamResult::Dropped => {
                // Deliver buffered bytes before resolving the terminal result.
                if !data.is_empty() {
                    return (Some((reader, done)), Ok(data));
                }
                return (None, done.await.map(|()| data).map_err(error));
            }
            StreamResult::Cancelled => return (None, Err(io::ErrorKind::Interrupted.into())),
        }
    }
}

async fn write_chunk((mut writer, done): Output, data: Vec<u8>) -> WriteResult {
    let remaining = writer.write_all(data).await;
    if remaining.is_empty() {
        return (Some((writer, done)), Ok(()));
    }
    drop(writer);
    let result = match done.await {
        Err(e) => Err(error(e)),
        Ok(()) => Err(io::ErrorKind::BrokenPipe.into()),
    };
    (None, result)
}

impl Socket for P3Socket {
    fn try_read(&mut self, buf: &mut dyn ReadBuf) -> io::Result<usize> {
        let state = self.state.get_mut().unwrap();
        if !state.buffer.is_empty() {
            let n = buf.remaining_mut().min(state.buffer.len());
            buf.put_slice(&state.buffer.split_to(n));
            return Ok(n);
        }
        if let Some(e) = state.read_error.take() {
            return Err(e);
        }
        if state.eof {
            return Ok(0);
        }
        Err(blocked())
    }

    fn poll_read_ready(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // TLS may enqueue its handshake flight before waiting for a reply.
        // Drive both directions; do not require the whole write to finish first.
        if let Poll::Ready(Err(error)) = self.poll_flush(cx) {
            return Poll::Ready(Err(error));
        }
        let state = self.state.get_mut().unwrap();
        if !state.buffer.is_empty() || state.eof || state.read_error.is_some() {
            return Poll::Ready(Ok(()));
        }
        if state.reading.is_none() {
            let input = state.input.take().expect("read state");
            state.reading = Some(Box::pin(read_chunk(input)));
        }
        let (input, result) = ready!(state.reading.as_mut().unwrap().as_mut().poll(cx));
        state.reading = None;
        state.eof = input.is_none();
        state.input = input;
        match result {
            Ok(bytes) => state.buffer = bytes.into(),
            Err(e) => state.read_error = Some(e),
        }
        Poll::Ready(Ok(()))
    }

    fn try_write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let state = self.state.get_mut().unwrap();
        if state.writing.is_some() {
            return Err(blocked());
        }
        let output = state.output.take().ok_or(io::ErrorKind::BrokenPipe)?;
        let n = buf.len().min(16 * 1024);
        state.writing = Some(Box::pin(write_chunk(output, buf[..n].to_vec())));
        Ok(n)
    }

    fn poll_write_ready(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }

    fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let state = self.state.get_mut().unwrap();
        let Some(task) = state.writing.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        let (output, result) = ready!(task.as_mut().poll(cx));
        state.writing = None;
        state.output = output;
        Poll::Ready(result)
    }

    fn poll_shutdown(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.poll_flush(cx))?;
        let state = self.state.get_mut().unwrap();
        if state.shutdown.is_none() {
            let Some((writer, done)) = state.output.take() else {
                return Poll::Ready(Ok(()));
            };
            drop(writer);
            state.shutdown = Some(Box::pin(async move { done.await.map_err(error) }));
        }
        let result = ready!(state.shutdown.as_mut().unwrap().as_mut().poll(cx));
        state.shutdown = None;
        Poll::Ready(result)
    }
}
