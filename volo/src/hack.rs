pub trait Unwrap<T> {
    fn volo_unwrap(self) -> T;
}

impl<T> Unwrap<T> for Option<T> {
    fn volo_unwrap(self) -> T {
        #[cfg(not(feature = "unsafe_unchecked"))]
        return self.unwrap();

        #[cfg(feature = "unsafe_unchecked")]
        unsafe {
            self.unwrap_unchecked()
        }
    }
}

impl<T, E: std::fmt::Debug> Unwrap<T> for Result<T, E> {
    fn volo_unwrap(self) -> T {
        #[cfg(not(feature = "unsafe_unchecked"))]
        return self.unwrap();

        #[cfg(feature = "unsafe_unchecked")]
        unsafe {
            self.unwrap_unchecked()
        }
    }
}
