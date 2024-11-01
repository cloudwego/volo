use http::{header::USER_AGENT, HeaderValue, Request};
use motore::Service;

const VOLO_USER_AGENT: &str = "volo-user-agent";

/// A [`Service`] that adds the user-agent header for every request.
#[derive(Debug)]
pub struct UserAgent<T> {
    inner: T,
    user_agent: HeaderValue,
}

impl<T> UserAgent<T> {
    pub fn new(inner: T, user_agent: Option<HeaderValue>) -> Self {
        let user_agent = user_agent
            .map(|value| {
                let mut buf = Vec::new();
                buf.extend(value.as_bytes());
                buf.push(b' ');
                buf.extend(VOLO_USER_AGENT.as_bytes());
                HeaderValue::from_bytes(&buf).expect("user-agent should be valid")
            })
            .unwrap_or_else(|| HeaderValue::from_static(VOLO_USER_AGENT));

        Self { inner, user_agent }
    }
}

impl<T, ReqBody, Cx> Service<Cx, Request<ReqBody>> for UserAgent<T>
where
    T: Service<Cx, Request<ReqBody>> + Send + Sync,
    ReqBody: Send + 'static,
    Cx: Send,
{
    type Response = T::Response;
    type Error = T::Error;

    async fn call(
        &self,
        cx: &mut Cx,
        mut req: Request<ReqBody>,
    ) -> Result<Self::Response, Self::Error> {
        req.headers_mut()
            .insert(USER_AGENT, self.user_agent.clone());

        self.inner.call(cx, req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Svc;

    #[test]
    fn sets_default_if_no_custom_user_agent() {
        assert_eq!(
            UserAgent::new(Svc, None).user_agent,
            HeaderValue::from_static(VOLO_USER_AGENT)
        )
    }

    #[test]
    fn prepends_custom_user_agent_to_default() {
        assert_eq!(
            UserAgent::new(Svc, Some(HeaderValue::from_static("Greeter 1.1"))).user_agent,
            HeaderValue::from_str(&format!("Greeter 1.1 {VOLO_USER_AGENT}")).unwrap()
        )
    }
}
