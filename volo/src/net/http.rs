use std::{sync::{Arc, Mutex}, time::Duration, io};


use futures::Future;
use pin_project::pin_project;
use reqwest::{header::HeaderMap};
use tokio::io::{DuplexStream, AsyncRead, AsyncWrite, AsyncReadExt, AsyncWriteExt};

use super::{Address, dial::{MakeTransport, Config}};

const WINDOW_SIZE: usize = 0x4000; // 4 * 4

struct WaitFlushed {
    inner: Arc<Mutex<bool>>,
}

impl Future for WaitFlushed {
    type Output = ();

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        cx.waker().wake_by_ref();
        let val = self.inner.lock().unwrap();
        // println!("fut check {val}");
        if !*val {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    }
}

#[derive(Clone)]
pub struct HttpMeta {
    is_flushed: Arc<Mutex<bool>>,
    address: Option<hyper::Uri>,
}

impl Default for HttpMeta {
    fn default() -> Self {
        Self {
            address: None,
            is_flushed: Arc::new(Mutex::new(false)),
        }
    }
}

impl HttpMeta {
    pub fn with_address(mut self, addr: Address) -> Self {
        if let Address::Http(url) = addr {
            self.address = Some(url);
        }
        self
    }
    
    pub fn get_url(&self) -> Option<hyper::Uri> {
        self.address.clone()
    }

    pub fn get_address(&self) -> Option<Address> {
        self.address.clone()
            .map(|addr| 
                Address::Http(addr))
    }

    pub fn wait_flushed(&self) -> impl Future<Output = ()> {
        WaitFlushed {
            inner: self.is_flushed.clone(),
        }
    }
}

#[pin_project]
pub struct HttpStream<R, W> {
    #[pin]
    reader: R,
    #[pin]
    writer: W,

    pub meta: HttpMeta,
}

type HttpReadHalfInternal = tokio::io::ReadHalf<DuplexStream>;
type HttpWriteHalfInternal = tokio::io::WriteHalf<DuplexStream>;
pub type Http = HttpStream<HttpReadHalfInternal, HttpWriteHalfInternal>;

pub type HttpReadHalf = tokio::io::ReadHalf<Http>;
pub type HttpWriteHalf = tokio::io::WriteHalf<Http>;

impl Http
{
    pub fn new(stream: DuplexStream) -> Self {
        let (rd, wr) = tokio::io::split(stream);
        Self {
            reader: rd,
            writer: wr,
            meta: HttpMeta::default(),
        }
    }

    // pub fn get_meta(&self) -> HttpMeta {
    //     self.meta.clone()
    // }

    pub fn with_meta(mut self, meta: HttpMeta) -> Self {
        self.meta = meta;
        self
    }
}

impl AsyncRead for Http
{
    fn poll_read(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
        self.project().reader.poll_read(cx, buf)
    }
}

impl AsyncWrite for Http
{
    fn poll_write(
                self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
                buf: &[u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
        self.project().writer.poll_write(cx, buf)
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        let this = self.project();
        let result = this.writer.poll_flush(cx);
        if result.is_ready() {
            *this.meta.is_flushed.lock().unwrap() = true;
        }
        result
    }

    fn poll_shutdown(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), std::io::Error>> {
        self.project().writer.poll_shutdown(cx)
    }
}

#[derive(Clone)]
pub struct HttpTransport {
    headers: Option<HeaderMap>,

    connect_timeout: Option<Duration>,
    read_timeout: Option<Duration>,
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self {
            connect_timeout: None,
            read_timeout: None,
            headers: None,
        }
    }
}

impl HttpTransport {
    fn new(cfg: &Config) -> Self {
        Self {
            connect_timeout: cfg.connect_timeout,
            read_timeout: cfg.read_timeout,
            headers: cfg.headers.clone(),
            ..Default::default()
        }
    }

    fn builder(&self) -> reqwest::ClientBuilder {
        reqwest::Client::builder()
    }
    fn build_client(&self) -> reqwest::Client {
        let mut builder = self.builder();
        if let Some(timeout) = self.connect_timeout {
            builder = builder.connect_timeout(timeout);
        }
        if let Some(timeout) = self.read_timeout {
            builder = builder.timeout(timeout)
        }
        builder.build().unwrap()
    }
    fn get_headers(&self) -> Option<HeaderMap> {
        self.headers.clone()
    }

    async fn make_transport_conn(
        &self,
        addr: Address,
    ) -> std::io::Result<Http> {
        let (cs, mut sc) = tokio::io::duplex(WINDOW_SIZE);
        let cs = Http::new(cs)
            .with_meta(HttpMeta::default()
                .with_address(addr));

        let client = self.build_client();
        let headers = self.get_headers();
        let meta = cs.meta.clone();
        
        tokio::spawn(async move {
            let mut payload = Vec::with_capacity(WINDOW_SIZE);
            let url = meta.get_url().unwrap();
            
            meta.wait_flushed().await;
            match sc.read_buf(&mut payload).await {
                Ok(siz) => {
                    if siz == 0 {
                        eprintln!("got transport error EOF");
                        return;
                    }
                    // println!("rpc_payload = {:?}", payload);
                    // println!("headers = {:?}", headers);
                    let mut req = client.post(url.to_string());
                    if let Some(headers) = headers {
                        req = req.headers(headers)
                    }
                    let req = req.body(reqwest::Body::from(payload))
                        .build()
                        .unwrap()
                    ;
                    let resp = client.execute(req).await;
                    match resp {
                        Ok(mut resp) => {
                            while let Ok(Some(chunk)) = resp.chunk().await {
                                if let Err(e) = sc.write_all(&chunk).await {
                                    eprintln!("got transport error response download {:#?}", e);
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("got transport error transmit {:#?}", e);
                            return;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("got transport error {:#?}", e);
                    return;
                }
            }
        });
        
        Ok(cs)
    }
}

impl MakeTransport for HttpTransport {
    type ReadHalf = HttpReadHalf;
    type WriteHalf = HttpWriteHalf;

    fn set_connect_timeout(&mut self, timeout: Option<std::time::Duration>) {
        self.connect_timeout = timeout;
    }

    fn set_read_timeout(&mut self, timeout: Option<std::time::Duration>) {
        self.read_timeout = timeout;
    }
    
    // set_write_timeout is no-op, write should be buffered.
    fn set_write_timeout(&mut self, _timeout: Option<std::time::Duration>) {
    }

    fn set_headers(&mut self, headers: Option<HeaderMap>) {
        self.headers = headers;
    }

    async fn make_transport(
            &self,
            addr: Address,
    ) -> std::io::Result<(Self::ReadHalf, Self::WriteHalf)>{
        let cs = self.make_transport_conn(addr).await?;
        let (csr, csw) = tokio::io::split(cs);
        Ok((csr, csw))
    }
}

// impl MakeIncoming for HttpTransport {
//     type Incoming = ();
//     async fn make_incoming(self) -> io::Result<Self::Incoming> {
//         todo!()
//     }
// }

pub async fn make_http_connection(cfg: &Config, url: hyper::Uri) -> Result<Http, io::Error> {
    let trans = HttpTransport::new(cfg);
    let cs = trans.make_transport_conn(Address::Http(url)).await?;
    Ok(cs)
}