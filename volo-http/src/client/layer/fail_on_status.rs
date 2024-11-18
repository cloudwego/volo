use std::{error::Error, fmt};

use http::status::StatusCode;
use motore::{layer::Layer, service::Service};
use url::Url;
use volo::context::Context;

use crate::{
    error::{client::request_error, ClientError},
    request::RequestPartsExt,
    response::Response,
};

/// [`Layer`] for throwing service error with the response's error status code.
///
/// Users can use [`FailOnStatus::all`], [`FailOnStatus::client_error`] or
/// [`FailOnStatus::server_error`] for creating the [`FailOnStatus`] layer that convert all (4XX and
/// 5XX), client error (4XX) or server error (5XX) to a error of service.
#[derive(Clone, Debug, Default)]
pub struct FailOnStatus {
    client_error: bool,
    server_error: bool,
    detailed: bool,
}

impl FailOnStatus {
    /// Create a [`FailOnStatus`] layer that return error [`StatusCodeError`] for all error status
    /// codes (4XX and 5XX).
    pub fn all() -> Self {
        Self {
            client_error: true,
            server_error: true,
            detailed: false,
        }
    }

    /// Create a [`FailOnStatus`] layer that return error [`StatusCodeError`] for client error
    /// status codes (4XX).
    pub fn client_error() -> Self {
        Self {
            client_error: true,
            server_error: false,
            detailed: false,
        }
    }

    /// Create a [`FailOnStatus`] layer that return error [`StatusCodeError`] for server error
    /// status codes (5XX).
    pub fn server_error() -> Self {
        Self {
            client_error: false,
            server_error: true,
            detailed: false,
        }
    }

    /// Collect more details in [`StatusCodeError`].
    ///
    /// When error occurs, the request has been consumed and the original response will be dropped.
    /// With this flag enabled, the layer will save more details in [`StatusCodeError`].
    pub fn detailed(mut self) -> Self {
        self.detailed = true;
        self
    }
}

impl<S> Layer<S> for FailOnStatus {
    type Service = FailOnStatusService<S>;

    fn layer(self, inner: S) -> Self::Service {
        FailOnStatusService {
            inner,
            fail_on: self,
        }
    }
}

/// The [`Service`] generated by [`FailOnStatus`] layer.
///
/// See [`FailOnStatus`] for more details.
pub struct FailOnStatusService<S> {
    inner: S,
    fail_on: FailOnStatus,
}

impl<Cx, Req, S, B> Service<Cx, Req> for FailOnStatusService<S>
where
    Cx: Context + Send,
    Req: RequestPartsExt + Send,
    S: Service<Cx, Req, Response = Response<B>, Error = ClientError> + Send + Sync,
{
    type Response = S::Response;
    type Error = S::Error;

    async fn call(&self, cx: &mut Cx, req: Req) -> Result<Self::Response, Self::Error> {
        let url = if self.fail_on.detailed {
            req.url()
        } else {
            None
        };
        let resp = self.inner.call(cx, req).await?;
        let status = resp.status();
        if (self.fail_on.client_error && status.is_client_error())
            || (self.fail_on.server_error && status.is_server_error())
        {
            Err(request_error(StatusCodeError { status, url })
                .with_endpoint(cx.rpc_info().callee()))
        } else {
            Ok(resp)
        }
    }
}

/// Client received a response with an error status code.
pub struct StatusCodeError {
    status: StatusCode,
    url: Option<Url>,
}

impl StatusCodeError {
    /// The original status code.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The target [`Url`]
    ///
    /// It will only be saved when [`FailOnStatus::detailed`] enabled.
    pub fn url(&self) -> Option<&Url> {
        self.url.as_ref()
    }
}

impl fmt::Debug for StatusCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatusCodeError")
            .field("status", &self.status)
            .finish()
    }
}

impl fmt::Display for StatusCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "client received an error status `{}`", self.status)?;
        if let Some(url) = &self.url {
            write!(f, " for `{url}`")?;
        }
        Ok(())
    }
}

impl Error for StatusCodeError {}

#[cfg(test)]
mod fail_on_status_tests {
    use http::status::StatusCode;
    use motore::service::Service;

    use super::FailOnStatus;
    use crate::{
        body::Body, client::test_helpers::MockTransport, context::ClientContext,
        error::ClientError, request::Request, response::Response, ClientBuilder,
    };

    struct ReturnStatus;

    impl Service<ClientContext, Request> for ReturnStatus {
        type Response = Response;
        type Error = ClientError;

        fn call(
            &self,
            _: &mut ClientContext,
            req: Request,
        ) -> impl std::future::Future<Output = Result<Self::Response, Self::Error>> + Send {
            let path = req.uri().path();
            assert_eq!(&path[..1], "/");
            let status_code = path[1..].parse::<u16>().expect("invalid uri");
            let status_code = StatusCode::from_u16(status_code).expect("invalid status code");
            let mut resp = Response::new(Body::empty());
            *resp.status_mut() = status_code;
            async { Ok(resp) }
        }
    }

    #[tokio::test]
    async fn fail_on_status_test() {
        {
            // Reject all error status codes
            let client = ClientBuilder::new()
                .layer_outer_front(FailOnStatus::all())
                .mock(MockTransport::service(ReturnStatus))
                .unwrap();
            client.get("/400").send().await.unwrap_err();
            client.get("/500").send().await.unwrap_err();
        }
        {
            // Reject client error status codes
            let client = ClientBuilder::new()
                .layer_outer_front(FailOnStatus::client_error())
                .mock(MockTransport::service(ReturnStatus))
                .unwrap();
            client.get("/400").send().await.unwrap_err();
            // 5XX is server error, it should not be handled
            client.get("/500").send().await.unwrap();
        }
        {
            // Reject all error status codes
            let client = ClientBuilder::new()
                .layer_outer_front(FailOnStatus::server_error())
                .mock(MockTransport::service(ReturnStatus))
                .unwrap();
            // 4XX is client error, it should not be handled
            client.get("/400").send().await.unwrap();
            client.get("/500").send().await.unwrap_err();
        }
    }
}
