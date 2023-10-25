macro_rules! cfg_rustls {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "rustls")]
            #[doc(cfg(feature = "rustls"))]
            $item
        )*
    }
}

macro_rules! cfg_native_tls {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "native-tls")]
            #[doc(cfg(feature = "native-tls"))]
            $item
        )*
    }
}

macro_rules! cfg_rustls_or_native_tls {
    ($($item:item)*) => {
        $(
            #[cfg(any(feature = "rustls", feature = "native-tls"))]
            #[doc(cfg(any(feature = "rustls", feature = "native-tls")))]
            $item
        )*
    }
}