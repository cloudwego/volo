#[macro_export]
macro_rules! volo_unreachable {
    () => {
        #[cfg(not(feature = "unsafe_unchecked"))]
        unreachable!();

        #[cfg(feature = "unsafe_unchecked")]
        unsafe {
            std::hint::unreachable_unchecked();
        }
    };
    ($msg:expr $(,)?) => ({
        unreachable!($msg)
    });
    ($fmt:expr, $($arg:tt)*) => ({
        unreachable!($fmt, $($arg)*)
    });
}

#[macro_export]
macro_rules! include_service {
    ($service: tt) => {
        include!(concat!(env!("OUT_DIR"), concat!("/", $service)));
    };
}

#[macro_export]
macro_rules! new_type {
    ($($(#[$attrs:meta])* $v:vis struct $name:ident($inner_v:vis $inner:ty);)+) => {
        $(
            $crate::new_type!(
                @attrs        [$(#[$attrs])*]
                @type         [$name]
                @inner        [$inner]
                @vis          [$v]
                @inner_vis    [$inner_v]
            );
        )+
    };



    (
        @attrs        [$(#[$attrs:meta])*]
        @type         [$type:ident]
        @inner        [$inner:ty]
        @vis          [$v:vis]
        @inner_vis    [$inner_v:vis]
    ) => {

        $(#[$attrs])*
        $v struct $type($inner_v $inner);

        impl<T> From<T> for $type where T: Into<$inner> {
            #[inline]
            fn from(t: T) -> Self {
                $type(t.into())
            }
        }

        impl ::std::ops::Deref for $type {
            type Target = $inner;

            #[inline]
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }
    }
}
