use motore::{service::Service, BoxError};

use crate::{context::ServerContext, ApplicationError, ApplicationErrorKind, Error};

pub async fn handle<Svc, Req, Resp>(
    cx: &mut ServerContext,
    svc: &mut Svc,
    req: Req,
) -> Result<Resp, Error>
where
    Svc: Service<ServerContext, Req, Response = Resp>,
    Svc::Error: Into<BoxError>,
{
    match svc.call(cx, req).await {
        Ok(resp) => Ok(resp),
        Err(e) => match e.into().downcast::<Error>() {
            Ok(e) => Err(*e),
            Err(e) => Err(Error::Application(ApplicationError::new(
                ApplicationErrorKind::Unknown,
                e.to_string(),
            ))),
        },
    }
}
