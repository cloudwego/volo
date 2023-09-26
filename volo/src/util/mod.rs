pub mod buf_reader;

use std::{borrow::Borrow, fmt, sync::Arc};

#[derive(Debug, PartialEq, PartialOrd, Eq, Hash)]
pub enum Ref<'a, B: ?Sized> {
    Borrowed(&'a B),
    Arc(Arc<B>),
}

impl<B: ?Sized> fmt::Display for Ref<'_, B>
where
    B: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        B::fmt(self, f)
    }
}

impl<B: ?Sized> Clone for Ref<'_, B> {
    fn clone(&self) -> Self {
        match self {
            Self::Borrowed(arg0) => Self::Borrowed(*arg0),
            Self::Arc(arg0) => Self::Arc(arg0.clone()),
        }
    }
}

impl<B: ?Sized> std::ops::Deref for Ref<'_, B> {
    type Target = B;

    fn deref(&self) -> &Self::Target {
        match self {
            Ref::Borrowed(b) => b,
            Ref::Arc(b) => b,
        }
    }
}

impl<B: ?Sized> Borrow<B> for Ref<'_, B> {
    fn borrow(&self) -> &B {
        match self {
            Ref::Borrowed(b) => b,
            Ref::Arc(b) => b,
        }
    }
}

impl<'a, B: ?Sized> From<&'a B> for Ref<'a, B> {
    fn from(b: &'a B) -> Self {
        Self::Borrowed(b)
    }
}

impl<B: ?Sized> From<Arc<B>> for Ref<'static, B> {
    fn from(b: Arc<B>) -> Self {
        Self::Arc(b)
    }
}
