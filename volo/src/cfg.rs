macro_rules! cfg_rustls {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "rustls")]
            #[cfg_attr(docsrs, doc(cfg(feature = "rustls")))]
            $item
        )*
    }
}

macro_rules! cfg_native_tls {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "native-tls")]
            #[cfg_attr(docsrs, doc(cfg(feature = "native-tls")))]
            $item
        )*
    }
}

macro_rules! cfg_rustls_or_native_tls {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "__tls")]
            #[cfg_attr(docsrs, doc(cfg(feature = "__tls")))]
            $item
        )*
    }
}
