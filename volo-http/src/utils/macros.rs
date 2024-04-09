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

macro_rules! impl_getter {
    ($name: ident, $type: ty, $($path: tt).+) => {
        paste::paste! {
            #[inline]
            pub fn $name(&self) -> &$type {
                &self.$($path).+
            }

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

#[allow(unused_macros)]
macro_rules! stat_impl {
    ($t: ident) => {
        paste::paste! {
            /// This is unstable now and may be changed in the future.
            #[inline]
            pub fn $t(&self) -> Option<DateTime<Local>> {
                self.$t
            }

            /// This is unstable now and may be changed in the future.
            #[doc(hidden)]
            #[inline]
            pub fn [<set_$t>](&mut self, t: DateTime<Local>) {
                self.$t = Some(t)
            }

            /// This is unstable now and may be changed in the future.
            #[inline]
            pub fn [<record_ $t>](&mut self) {
                self.$t = Some(Local::now())
            }
        }
    };
}

pub(crate) use all_the_tuples;
pub(crate) use all_the_tuples_with_special_case;
pub(crate) use impl_deref_and_deref_mut;
pub(crate) use impl_getter;
pub(crate) use stat_impl;
