use std::convert::Into;
use std::ops::{Deref, DerefMut, Drop};
use zeroize::Zeroize;

/// String that is zeroed when dropped
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafeString {
    inner: String,
}

impl SafeString {
    pub fn new() -> SafeString {
        SafeString {
            inner: String::new(),
        }
    }

    pub fn from_string(inner: String) -> SafeString {
        SafeString { inner }
    }

    pub fn into_inner(mut self) -> String {
        std::mem::replace(&mut self.inner, String::new())
    }
}

impl Drop for SafeString {
    fn drop(&mut self) {
        // Wipe the allocation, including characters removed by terminal editing.
        self.inner.zeroize();
    }
}

impl Deref for SafeString {
    type Target = String;

    fn deref(&self) -> &String {
        &self.inner
    }
}

impl DerefMut for SafeString {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl Into<SafeString> for String {
    fn into(self) -> SafeString {
        SafeString::from_string(self)
    }
}

impl<'a> Into<SafeString> for &'a str {
    fn into(self) -> SafeString {
        self.to_string().into()
    }
}
