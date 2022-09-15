use std::{marker::PhantomData, net::SocketAddr, str::FromStr};

use futures::Future;
use http::{
    header::{CONTENT_TYPE, TE},
    HeaderMap, HeaderValue,
};
use hyper::{client::HttpConnector, Client as HyperClient};
use hyper_timeout::TimeoutConnector;
use metainfo::{Backward, Forward};
use motore::Service;
use tower::{util::ServiceExt, Service as TowerService};
use volo::{context::Context, net::Address, Unwrap};

use crate::{
    client::Http2Config,
    codec::decode::Kind,
    context::{ClientContext, Config},
    metadata::{
        MetadataKey, MetadataMap, DESTINATION_ADDR, DESTINATION_METHOD, DESTINATION_SERVICE,
        HEADER_TRANS_REMOTE_ADDR, SOURCE_SERVICE,
    },
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

            let (mut metadata, extensions, message) = volo_req.into_parts();

            insert_metadata(&mut metadata, cx)?;

            let path = cx.rpc_info.method().volo_unwrap();
            let body = hyper::Body::wrap_stream(message.into_body());
            let mut req = hyper::Request::new(body);
            *req.version_mut() = http::Version::HTTP_2;
            *req.method_mut() = http::Method::POST;
            *req.uri_mut() = build_uri(target, path);
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
            extract_metadata(&parts.headers, cx)?;

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
        #[cfg(target_family = "unix")]
        Address::Unix(unix) => hyper::Uri::builder()
            .scheme("http+unix")
            .authority(unix.display().to_string())
            .path_and_query(path)
            .build()
            .expect("fail to build unix uri"),
    }
}

fn insert_metadata(metadata: &mut MetadataMap, cx: &mut ClientContext) -> Result<(), Status> {
    metainfo::METAINFO.with(|metainfo| {
        let metainfo = metainfo.borrow_mut();

        // persistents for multi-hops
        if let Some(ap) = metainfo.get_all_persistents() {
            for (key, value) in ap {
                let key = metainfo::HTTP_PREFIX_PERSISTENT.to_owned() + key;
                metadata.insert(
                    MetadataKey::from_str(key.as_str())
                        .map_err(|err| Status::from_error(Box::new(err)))?,
                    value
                        .parse()
                        .map_err(|err| Status::from_error(Box::new(err)))?,
                );
            }
        }

        // transients for one-hop
        if let Some(at) = metainfo.get_all_transients() {
            for (key, value) in at {
                let key = metainfo::HTTP_PREFIX_TRANSIENT.to_owned() + key;
                metadata.insert(
                    MetadataKey::from_str(key.as_str())
                        .map_err(|err| Status::from_error(Box::new(err)))?,
                    value
                        .parse()
                        .map_err(|err| Status::from_error(Box::new(err)))?,
                );
            }
        }

        // caller
        if let Some(caller) = cx.rpc_info.caller.as_ref() {
            metadata.insert(
                SOURCE_SERVICE,
                caller
                    .service_name()
                    .parse()
                    .map_err(|err| Status::from_error(Box::new(err)))?,
            );
        }

        // callee
        if let Some(callee) = cx.rpc_info.callee.as_ref() {
            metadata.insert(
                DESTINATION_SERVICE,
                callee
                    .service_name()
                    .parse()
                    .map_err(|err| Status::from_error(Box::new(err)))?,
            );
            if let Some(method) = cx.rpc_info.method() {
                metadata.insert(
                    DESTINATION_METHOD,
                    method
                        .parse()
                        .map_err(|err| Status::from_error(Box::new(err)))?,
                );
            }
            if let Some(addr) = callee.address() {
                metadata.insert(
                    DESTINATION_ADDR,
                    addr.to_string()
                        .parse()
                        .map_err(|err| Status::from_error(Box::new(err)))?,
                );
            }
        }

        Ok::<(), Status>(())
    })
}

fn extract_metadata(
    headers: &HeaderMap<HeaderValue>,
    cx: &mut ClientContext,
) -> Result<(), Status> {
    metainfo::METAINFO.with(|metainfo| {
        let mut metainfo = metainfo.borrow_mut();

        // callee
        if let Some(ad) = headers.get(HEADER_TRANS_REMOTE_ADDR) {
            let maybe_addr = ad
                .to_str()
                .map_err(|err| Status::from_error(Box::new(err)))?
                .parse::<SocketAddr>();
            if let (Some(callee), Ok(addr)) = (cx.rpc_info_mut().callee.as_mut(), maybe_addr) {
                callee.set_address(volo::net::Address::from(addr));
            }
        }

        // backward
        for (k, v) in headers.into_iter() {
            let k = k.as_str();
            let v = v.to_str().map_err(|err| Status::from_error(err.into()))?;
            if k.starts_with(metainfo::HTTP_PREFIX_BACKWARD) {
                metainfo.strip_rpc_prefix_and_set_backward_downstream(k.to_owned(), v.to_owned());
            }
        }

        Ok::<(), Status>(())
    })
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
