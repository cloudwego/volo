use std::marker::PhantomData;

use futures::Future;
use http::{
    header::{CONTENT_TYPE, TE},
    HeaderValue,
};
use hyper::{client::HttpConnector, Client as HyperClient};
use hyper_timeout::TimeoutConnector;
use motore::Service;
use tower::{util::ServiceExt, Service as TowerService};
use volo::{net::Address, Unwrap};

use crate::{
    client::Http2Config,
    codec::decode::Kind,
    context::{ClientContext, Config},
    Code, Request, Response, Status,
};

/// A simple wrapper of [`hyper::client::client`] that implements [`Service`]
/// to make outgoing requests.
pub struct ClientTransport<U> {
    http_client: HyperClient<TimeoutConnector<HttpConnector>>,
    _marker: PhantomData<fn(U)>,
}

impl<U> Clone for ClientTransport<U> {
    fn clone(&self) -> Self {
        Self {
            http_client: self.http_client.clone(),
            _marker: self._marker,
        }
    }
}

impl<U> ClientTransport<U> {
    /// Creates a new [`ClientTransport`] by setting the underlying connection
    /// with the given config.
    pub fn new(http2_config: &Http2Config, rpc_config: &Config) -> Self {
        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        connector.set_nodelay(http2_config.tcp_nodelay);
        connector.set_keepalive(http2_config.tcp_keepalive);
        let mut connector = TimeoutConnector::new(connector);
        if let Some(connect_timeout) = rpc_config.connect_timeout {
            connector.set_connect_timeout(Some(connect_timeout));
            connector.set_read_timeout(rpc_config.read_timeout);
            connector.set_write_timeout(rpc_config.write_timeout);
        }

        let http = HyperClient::builder()
            .http2_only(!http2_config.accept_http1)
            .http2_initial_stream_window_size(http2_config.init_stream_window_size)
            .http2_initial_connection_window_size(http2_config.init_connection_window_size)
            .http2_max_frame_size(http2_config.max_frame_size)
            .http2_adaptive_window(http2_config.adaptive_window)
            .http2_keep_alive_interval(http2_config.http2_keepalive_interval)
            .http2_keep_alive_timeout(http2_config.http2_keepalive_timeout)
            .http2_keep_alive_while_idle(http2_config.http2_keepalive_while_idle)
            .http2_max_concurrent_reset_streams(http2_config.max_concurrent_reset_streams)
            .retry_canceled_requests(http2_config.retry_canceled_requests)
            .build(connector);

        ClientTransport {
            http_client: http,
            _marker: PhantomData,
        }
    }
}

impl<T, U> Service<ClientContext, Request<T>> for ClientTransport<U>
where
    T: crate::message::SendEntryMessage + Send + 'static,
    U: crate::message::RecvEntryMessage + 'static,
{
    type Response = Response<U>;

    type Error = Status;

    type Future<'cx> = impl Future<Output = Result<Self::Response, Self::Error>>;

    fn call<'cx, 's>(
        &'s mut self,
        cx: &'cx mut ClientContext,
        volo_req: Request<T>,
    ) -> Self::Future<'cx>
    where
        's: 'cx,
    {
        let mut http_client = self.http_client.clone();
        async move {
            // SAFETY: parameters controlled by volo-grpc are guaranteed to be valid.
            // get the call address from the context
            let target = cx
                .rpc_info
                .callee()
                .volo_unwrap()
                .address()
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "address is required")
                })?;
            let path = cx.rpc_info.method().volo_unwrap();

            let (metadata, extensions, message) = volo_req.into_parts();
            let body = message.into_body();
            let body = hyper::Body::wrap_stream(body);

            let mut req = hyper::Request::new(body);
            *req.version_mut() = http::Version::HTTP_2;
            *req.method_mut() = http::Method::POST;
            *req.uri_mut() = build_uri(target, path.as_str());
            *req.headers_mut() = metadata.into_headers();
            *req.extensions_mut() = extensions;
            req.headers_mut()
                .insert(TE, HeaderValue::from_static("trailers"));
            req.headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/grpc"));

            // call the service through hyper client
            let resp = http_client
                .ready()
                .await
                .map_err(|err| Status::from_error(err.into()))?
                .call(req)
                .await
                .map_err(|err| Status::from_error(err.into()))?;

            let status_code = resp.status();
            if let Some(status) = Status::from_header_map(resp.headers()) {
                if status.code() != Code::Ok {
                    return Err(status);
                }
            }
            let (parts, body) = resp.into_parts();
            let body = U::from_body(Some(path), body, Kind::Response(status_code))?;
            let resp = hyper::Response::from_parts(parts, body);

            Ok(Response::from_http(resp))
        }
    }
}

fn build_uri(addr: Address, path: &str) -> hyper::Uri {
    match addr {
        Address::Ip(ip) => hyper::Uri::builder()
            .scheme(http::uri::Scheme::HTTP)
            .authority(ip.to_string())
            .path_and_query(path)
            .build()
            .expect("fail to build ip uri"),
        Address::Unix(unix) => hyper::Uri::builder()
            .scheme("http+unix")
            .authority(unix.display().to_string())
            .path_and_query(path)
            .build()
            .expect("fail to build unix uri"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_build_uri() {
        let addr = "127.0.0.1:8000".parse::<std::net::SocketAddr>().unwrap();
        let path = "/path?query=1";
        let uri = "http://127.0.0.1:8000/path?query=1"
            .parse::<hyper::Uri>()
            .unwrap();
        assert_eq!(super::build_uri(volo::net::Address::from(addr), path), uri);
    }
}
