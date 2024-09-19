#[cfg(feature = "server")]
#[rustfmt::skip]
macro_rules! all_the_tuples_with_special_case {
    ($name:ident) => {
        $name!([], T1);
        $name!([T1], T2);
        $name!([T1, T2], T3);
        $name!([T1, T2, T3], T4);
        $name!([T1, T2, T3, T4], T5);
        $name!([T1, T2, T3, T4, T5], T6);
        $name!([T1, T2, T3, T4, T5, T6], T7);
        $name!([T1, T2, T3, T4, T5, T6, T7], T8);
        $name!([T1, T2, T3, T4, T5, T6, T7, T8], T9);
        $name!([T1, T2, T3, T4, T5, T6, T7, T8, T9], T10);
        $name!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10], T11);
        $name!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11], T12);
        $name!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12], T13);
        $name!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13], T14);
        $name!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14], T15);
        $name!([T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15], T16);
    };
}
#[cfg(feature = "server")]
pub(crate) use all_the_tuples_with_special_case;

#[cfg(feature = "server")]
#[rustfmt::skip]
macro_rules! all_the_tuples {
    ($name:ident) => {
        $name!(T1);
        $name!(T1, T2);
        $name!(T1, T2, T3);
        $name!(T1, T2, T3, T4);
        $name!(T1, T2, T3, T4, T5);
        $name!(T1, T2, T3, T4, T5, T6);
        $name!(T1, T2, T3, T4, T5, T6, T7);
        $name!(T1, T2, T3, T4, T5, T6, T7, T8);
        $name!(T1, T2, T3, T4, T5, T6, T7, T8, T9);
        $name!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10);
        $name!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11);
        $name!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12);
        $name!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13);
        $name!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14);
        $name!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15);
        $name!(T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15, T16);
    };
}
#[cfg(feature = "server")]
pub(crate) use all_the_tuples;

#[cfg(any(feature = "client", feature = "server"))]
macro_rules! impl_deref_and_deref_mut {
    ($type:ty, $inner:ty, $pos:tt) => {
        impl std::ops::Deref for $type {
            type Target = $inner;

            #[inline]
            fn deref(&self) -> &Self::Target {
                &self.$pos
            }
        }

        impl std::ops::DerefMut for $type {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.$pos
            }
        }
    };
}
#[cfg(any(feature = "client", feature = "server"))]
pub(crate) use impl_deref_and_deref_mut;

#[cfg(feature = "server")]
macro_rules! impl_getter {
    ($name: ident, $type: ty, $($path: tt).+) => {
        paste::paste! {
            #[doc = "Get a reference to [`" $type "`]"]
            #[inline]
            pub fn $name(&self) -> &$type {
                &self.$($path).+
            }

            #[doc = "Get a mutable reference to [`" $type "`]"]
            #[inline]
            pub fn [<$name _mut>](&mut self) -> &mut $type {
                &mut self.$($path).+
            }
        }
    };
    ($name: ident, $type: ty) => {
        impl_getter!($name, $type, $name);
    };
}
#[cfg(feature = "server")]
pub(crate) use impl_getter;

#[cfg(feature = "client")]
macro_rules! stat_impl {
    ($t: ident) => {
        paste::paste! {
            #[doc = "Get the recorded [`DateTime`] of \"" $t "\""]
            #[inline]
            pub fn $t(&self) -> Option<DateTime<Local>> {
                self.$t
            }

            #[doc = "Set a [`DateTime`] of \"" $t "\""]
            #[doc(hidden)]
            #[inline]
            pub fn [<set_$t>](&mut self, t: DateTime<Local>) {
                self.$t = Some(t)
            }

            #[doc = "Record the current [`DateTime`] of \"" $t "\""]
            #[inline]
            pub fn [<record_ $t>](&mut self) {
                self.$t = Some(Local::now())
            }
        }
    };
}
#[cfg(feature = "client")]
pub(crate) use stat_impl;
