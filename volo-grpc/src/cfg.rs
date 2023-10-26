#[allow(unused_macros)] // Otherwise, it will complain if neither `rustls` nor `native-tls` is enabled.
macro_rules! cfg_rustls {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "rustls")]
            #[doc(cfg(feature = "rustls"))]
            $item
        )*
    }
}

#[allow(unused_macros)] // Otherwise, it will complain if neither `rustls` nor `native-tls` is enabled.
macro_rules! cfg_native_tls {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "native-tls")]
            #[doc(cfg(feature = "native-tls"))]
            $item
        )*
    }
}

#[allow(unused_macros)] // Otherwise, it will complain if neither `rustls` nor `native-tls` is enabled.
macro_rules! cfg_rustls_or_native_tls {
    ($($item:item)*) => {
        $(
            #[cfg(any(feature = "rustls", feature = "native-tls"))]
            #[doc(cfg(any(feature = "rustls", feature = "native-tls")))]
            $item
        )*
    }
}
