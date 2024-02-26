#[allow(unused_macros)] // Otherwise, it will complain if neither `rustls` nor `native-tls` is enabled.
macro_rules! cfg_rustls {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "rustls")]
            #[cfg_attr(docsrs, doc(cfg(feature = "rustls")))]
            $item
        )*
    }
}

#[allow(unused_macros)] // Otherwise, it will complain if neither `rustls` nor `native-tls` is enabled.
macro_rules! cfg_native_tls {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "native-tls")]
            #[cfg_attr(docsrs, doc(cfg(feature = "native-tls")))]
            $item
        )*
    }
}

#[allow(unused_macros)] // Otherwise, it will complain if neither `rustls` nor `native-tls` is enabled.
macro_rules! cfg_rustls_or_native_tls {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "__tls")]
            #[cfg_attr(docsrs, doc(cfg(feature = "__tls")))]
            $item
        )*
    }
}
