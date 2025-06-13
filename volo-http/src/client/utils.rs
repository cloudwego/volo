use http::uri::Scheme;

use crate::utils::consts;

pub fn get_default_port(scheme: &Scheme) -> u16 {
    #[cfg(feature = "__tls")]
    if scheme == &Scheme::HTTPS {
        return consts::HTTPS_DEFAULT_PORT;
    }
    if scheme == &Scheme::HTTP {
        return consts::HTTP_DEFAULT_PORT;
    }
    unreachable!("[Volo-HTTP] https is not allowed when feature `tls` is not enabled")
}

pub fn is_default_port(scheme: &Scheme, port: u16) -> bool {
    get_default_port(scheme) == port
}
