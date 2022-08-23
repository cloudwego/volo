use futures::Future;
use http::{Request, Uri};
use motore::Service;

/// A [`Service`] that adds the origin header for every request.
#[derive(Debug)]
pub struct AddOrigin<T> {
    inner: T,
    origin: Uri,
}

impl<T> AddOrigin<T> {
    /// Create a new [`AddOrigin`] service.
    pub fn new(inner: T, origin: Uri) -> Self {
        Self { inner, origin }
    }
}

impl<T, ReqBody, Cx> Service<Cx, Request<ReqBody>> for AddOrigin<T>
where
    T: Service<Cx, Request<ReqBody>>,
    ReqBody: 'static,
{
    type Response = T::Response;
    type Error = T::Error;
    type Future<'cx> = impl Future<Output = Result<Self::Response, Self::Error>> + 'cx
    where
        Self: 'cx,
        Cx: 'cx;

    fn call<'cx, 's>(&'s mut self, cx: &'cx mut Cx, req: Request<ReqBody>) -> Self::Future<'cx>
    where
        's: 'cx,
    {
        // split the header and body
        let (mut head, body) = req.into_parts();

        // set new uri
        let mut uri: http::uri::Parts = head.uri.into();
        let set_uri = self.origin.clone().into_parts();

        uri.scheme = Some(set_uri.scheme.expect("expected scheme"));
        uri.authority = Some(set_uri.authority.expect("expected authority"));

        // update head.uri
        head.uri = http::Uri::from_parts(uri).expect("valid uri");

        // combine into http::Request
        let request = Request::from_parts(head, body);

        // call inner Service
        self.inner.call(cx, request)
    }
}
