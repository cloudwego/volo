pub mod buf_reader;

// used internally.
#[doc(hidden)]
pub mod server_remote_error;

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

#[cfg(test)]
mod tests {
    use std::{fmt, sync::Arc};

    use super::{Borrow, Ref};

    #[derive(Debug, PartialEq, Eq, Copy, Clone)]
    struct TestData;

    impl fmt::Display for TestData {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("TestData")
        }
    }

    #[test]
    fn test_borrowed_ref() {
        let data = TestData;
        let borrowed_ref: Ref<'_, TestData> = (&data).into();
        assert_eq!(*borrowed_ref, data);
    }

    #[test]
    fn test_arc_ref() {
        let data = TestData;
        let arc_ref: Ref<'static, TestData> = Arc::new(data).into();
        assert_eq!(*arc_ref, data);
    }

    #[test]
    fn test_display_trait() {
        let data = TestData;
        let borrowed_ref: Ref<'_, TestData> = (&data).into();
        assert_eq!(format!("{borrowed_ref}"), format!("{}", data));
    }

    #[test]
    fn test_clone_trait() {
        let data = TestData;
        let borrowed_ref: Ref<'_, TestData> = (&data).into();
        let cloned_ref = borrowed_ref.clone();
        assert_eq!(cloned_ref, borrowed_ref);
    }

    #[test]
    fn test_deref_trait() {
        let data = TestData;
        let borrowed_ref: Ref<'_, TestData> = (&data).into();
        assert_eq!(*borrowed_ref, data);
    }

    #[test]
    fn test_borrow_trait() {
        let data = TestData;
        let borrowed_ref: Ref<'_, TestData> = (&data).into();
        let borrowed: &TestData = borrowed_ref.borrow();
        assert_eq!(borrowed, &data);
    }
}
