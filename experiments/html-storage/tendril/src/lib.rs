// SPDX-License-Identifier: MIT OR Apache-2.0
//! App-focused experimental implementation of the UTF-8 and byte Tendril interfaces.
#![forbid(unsafe_code)]
use std::{
    cmp::Ordering,
    fmt as formatting,
    hash::{Hash, Hasher},
    ops::{Deref, Range},
    rc::Rc,
};

pub mod fmt {
    use super::Range;
    pub enum UTF8 {}
    pub enum Bytes {}
    mod sealed {
        pub trait Sealed {}
    }
    impl sealed::Sealed for UTF8 {}
    impl sealed::Sealed for Bytes {}
    pub trait Format: sealed::Sealed {
        type Slice: ?Sized + PartialEq + Eq + Ord + std::hash::Hash;
        type Buf: Clone;
        fn bytes(s: &Self::Slice) -> &[u8];
        fn decode(bytes: &[u8]) -> Option<&Self::Slice>;
        fn whole(b: &Self::Buf) -> &Self::Slice;
        fn view(s: &Self::Slice, range: Range<usize>) -> Option<&Self::Slice>;
        fn own(s: &Self::Slice, capacity: usize) -> Self::Buf;
        fn append(b: &mut Self::Buf, s: &Self::Slice);
        fn truncate(b: &mut Self::Buf, end: usize);
        fn reserve(b: &mut Self::Buf, additional: usize);
    }
    impl Format for UTF8 {
        type Slice = str;
        type Buf = String;
        #[inline]
        fn bytes(s: &str) -> &[u8] {
            s.as_bytes()
        }
        #[inline]
        fn decode(bytes: &[u8]) -> Option<&str> {
            std::str::from_utf8(bytes).ok()
        }
        #[inline]
        fn whole(b: &String) -> &str {
            b
        }
        #[inline]
        fn view(s: &str, r: Range<usize>) -> Option<&str> {
            s.get(r)
        }
        fn own(s: &str, cap: usize) -> String {
            let mut b = String::with_capacity(cap.max(s.len()));
            b.push_str(s);
            b
        }
        fn append(b: &mut String, s: &str) {
            b.push_str(s)
        }
        fn truncate(b: &mut String, end: usize) {
            b.truncate(end)
        }
        fn reserve(b: &mut String, a: usize) {
            b.reserve(a)
        }
    }
    impl Format for Bytes {
        type Slice = [u8];
        type Buf = Vec<u8>;
        #[inline]
        fn bytes(s: &[u8]) -> &[u8] {
            s
        }
        #[inline]
        fn decode(bytes: &[u8]) -> Option<&[u8]> {
            Some(bytes)
        }
        #[inline]
        fn whole(b: &Vec<u8>) -> &[u8] {
            b
        }
        #[inline]
        fn view(s: &[u8], r: Range<usize>) -> Option<&[u8]> {
            s.get(r)
        }
        fn own(s: &[u8], cap: usize) -> Vec<u8> {
            let mut b = Vec::with_capacity(cap.max(s.len()));
            b.extend_from_slice(s);
            b
        }
        fn append(b: &mut Vec<u8>, s: &[u8]) {
            b.extend_from_slice(s)
        }
        fn truncate(b: &mut Vec<u8>, end: usize) {
            b.truncate(end)
        }
        fn reserve(b: &mut Vec<u8>, a: usize) {
            b.reserve(a)
        }
    }
}
pub use fmt::Format;
pub mod stream;
pub use stream::TendrilSink;
const INLINE: usize = 8;
enum Storage<F: Format> {
    Inline([u8; INLINE]),
    Heap(Rc<F::Buf>, std::cell::Cell<bool>),
}
pub struct Tendril<F: Format> {
    storage: Storage<F>,
    start: u32,
    end: u32,
}
pub type StrTendril = Tendril<fmt::UTF8>;
pub type ByteTendril = Tendril<fmt::Bytes>;
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SubtendrilError {
    OutOfBounds,
    ValidationFailed,
}

impl<F: Format> Tendril<F> {
    #[inline]
    pub fn new() -> Self {
        Self {
            storage: Storage::Inline([0; INLINE]),
            start: 0,
            end: 0,
        }
    }
    #[inline]
    pub fn from_slice(s: &F::Slice) -> Self {
        let b = F::bytes(s);
        let end = u32::try_from(b.len()).expect("tendril length overflow");
        let storage = if b.len() <= INLINE {
            let mut a = [0; INLINE];
            a[..b.len()].copy_from_slice(b);
            Storage::Inline(a)
        } else {
            Storage::Heap(Rc::new(F::own(s, b.len())), false.into())
        };
        Self {
            storage,
            start: 0,
            end,
        }
    }
    #[allow(
        clippy::result_unit_err,
        reason = "Match the published conversion interface."
    )]
    pub fn try_from_byte_slice(b: &[u8]) -> Result<Self, ()> {
        F::decode(b).map(Self::from_slice).ok_or(())
    }
    pub fn with_capacity(n: u32) -> Self {
        let mut x = Self::new();
        x.reserve(n);
        x
    }
    #[inline]
    pub fn len32(&self) -> u32 {
        self.end - self.start
    }
    pub fn clear(&mut self) {
        if let Storage::Heap(b, shared) = &mut self.storage
            && !shared.get()
        {
            F::truncate(Rc::get_mut(b).unwrap(), 0);
            self.start = 0;
            self.end = 0;
            return;
        }
        *self = Self::new();
    }
    fn kind(&self) -> &'static str {
        match &self.storage {
            Storage::Inline(_) => "inline",
            Storage::Heap(_, shared) if shared.get() => "shared",
            Storage::Heap(_, _) => "owned",
        }
    }
    #[inline]
    fn range(&self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
    // Detach only the visible view. A consumed prefix is compacted only after at least
    // as many bytes have been discarded as remain, amortizing append/consume cycles.
    fn prepare_heap(&mut self, additional: u32) {
        let len = self.len32();
        let capacity = len
            .checked_add(additional)
            .expect("tendril length overflow");
        let copy = match &self.storage {
            Storage::Inline(_) => true,
            Storage::Heap(b, _) => {
                Rc::strong_count(b) != 1
                    || self.end.checked_add(additional).is_none()
                    || (self.start >= 4096 && self.start >= len)
            }
        };
        if copy {
            let b = F::own(self, capacity as usize);
            self.storage = Storage::Heap(Rc::new(b), false.into());
            self.start = 0;
            self.end = len;
        } else if let Storage::Heap(b, shared) = &mut self.storage {
            shared.set(false);
            F::truncate(Rc::get_mut(b).unwrap(), self.end as usize);
        }
    }
    pub fn reserve(&mut self, additional: u32) {
        // A reservation is a hint. The published API leaves a shared view alone.
        if matches!(&self.storage, Storage::Heap(_, shared) if shared.get()) {
            return;
        }
        if self
            .len32()
            .checked_add(additional)
            .expect("tendril length overflow") as usize
            <= INLINE
        {
            return;
        }
        self.prepare_heap(additional);
        if let Storage::Heap(b, _) = &mut self.storage {
            F::reserve(Rc::get_mut(b).unwrap(), additional as usize);
        }
    }
    #[inline]
    pub fn push_slice(&mut self, s: &F::Slice) {
        let bytes = F::bytes(s);
        if bytes.is_empty() {
            return;
        }
        let n = u32::try_from(bytes.len()).expect("tendril length overflow");
        let len = self.len32();
        let new_len = len.checked_add(n).expect("tendril length overflow");
        if let Storage::Inline(b) = &mut self.storage
            && new_len as usize <= INLINE
        {
            if self.start != 0 {
                b.copy_within(self.start as usize..self.end as usize, 0);
            }
            b[len as usize..new_len as usize].copy_from_slice(bytes);
            self.start = 0;
            self.end = new_len;
            return;
        }
        self.prepare_heap(n);
        if let Storage::Heap(b, _) = &mut self.storage {
            F::append(Rc::get_mut(b).unwrap(), s);
        }
        self.end += n;
    }
    pub fn push_tendril(&mut self, s: &Self) {
        if s.len32() == 0 {
            return;
        }
        if let (Storage::Heap(a, _), Storage::Heap(b, _)) = (&self.storage, &s.storage)
            && self.end == s.start
            && Rc::ptr_eq(a, b)
        {
            self.end = s.end;
            return;
        }
        self.push_slice(s);
    }
    fn checked(&self, off: u32, len: u32) -> Result<&F::Slice, SubtendrilError> {
        let end = off
            .checked_add(len)
            .filter(|e| *e <= self.len32())
            .ok_or(SubtendrilError::OutOfBounds)?;
        if len == 0 {
            return Ok(F::decode(&[]).unwrap());
        }
        F::view(self, off as usize..end as usize).ok_or(SubtendrilError::ValidationFailed)
    }
    pub fn try_subtendril(&self, off: u32, len: u32) -> Result<Self, SubtendrilError> {
        let s = self.checked(off, len)?;
        if len as usize <= INLINE {
            return Ok(Self::from_slice(s));
        }
        let mut out = self.clone();
        out.start += off;
        out.end = out.start + len;
        Ok(out)
    }
    pub fn subtendril(&self, off: u32, len: u32) -> Self {
        self.try_subtendril(off, len).unwrap()
    }
    pub fn try_pop_front(&mut self, n: u32) -> Result<(), SubtendrilError> {
        if n == 0 {
            return Ok(());
        }
        let remain = self
            .len32()
            .checked_sub(n)
            .ok_or(SubtendrilError::OutOfBounds)?;
        let visible = self.checked(n, remain)?;
        if remain as usize <= INLINE {
            *self = Self::from_slice(visible)
        } else {
            if let Storage::Heap(_, flag) = &self.storage {
                flag.set(true);
            }
            self.start += n;
        }
        Ok(())
    }
    pub fn try_pop_back(&mut self, n: u32) -> Result<(), SubtendrilError> {
        if n == 0 {
            return Ok(());
        }
        let keep = self
            .len32()
            .checked_sub(n)
            .ok_or(SubtendrilError::OutOfBounds)?;
        let visible = self.checked(0, keep)?;
        if keep as usize <= INLINE {
            *self = Self::from_slice(visible)
        } else {
            if let Storage::Heap(_, flag) = &self.storage {
                flag.set(true);
            }
            self.end -= n;
        }
        Ok(())
    }
    pub fn pop_front(&mut self, n: u32) {
        self.try_pop_front(n).unwrap()
    }
    pub fn pop_back(&mut self, n: u32) {
        self.try_pop_back(n).unwrap()
    }
}
impl<F: Format> Default for Tendril<F> {
    fn default() -> Self {
        Self::new()
    }
}
impl<F: Format> Clone for Tendril<F> {
    fn clone(&self) -> Self {
        Self {
            storage: match &self.storage {
                Storage::Inline(b) => Storage::Inline(*b),
                Storage::Heap(b, shared) => {
                    shared.set(true);
                    Storage::Heap(b.clone(), true.into())
                }
            },
            start: self.start,
            end: self.end,
        }
    }
}
impl<F: Format> Deref for Tendril<F> {
    type Target = F::Slice;
    #[inline]
    fn deref(&self) -> &Self::Target {
        match &self.storage {
            Storage::Inline(b) => F::decode(&b[self.range()]).unwrap(),
            Storage::Heap(b, _) => F::view(F::whole(b), self.range()).unwrap(),
        }
    }
}
impl<F: Format> PartialEq for Tendril<F> {
    fn eq(&self, o: &Self) -> bool {
        **self == **o
    }
}
impl<F: Format> Eq for Tendril<F> {}
impl<F: Format> PartialOrd for Tendril<F> {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl<F: Format> Ord for Tendril<F> {
    fn cmp(&self, o: &Self) -> Ordering {
        self.deref().cmp(o.deref())
    }
}
impl<F: Format> Hash for Tendril<F> {
    fn hash<H: Hasher>(&self, h: &mut H) {
        F::bytes(self).hash(h)
    }
}
impl formatting::Debug for StrTendril {
    fn fmt(&self, f: &mut formatting::Formatter<'_>) -> formatting::Result {
        write!(f, "Tendril<UTF8>({}: {:?})", self.kind(), &**self)
    }
}
impl formatting::Debug for ByteTendril {
    fn fmt(&self, f: &mut formatting::Formatter<'_>) -> formatting::Result {
        write!(f, "Tendril<Bytes>({}: {:?})", self.kind(), &**self)
    }
}
impl formatting::Display for StrTendril {
    fn fmt(&self, f: &mut formatting::Formatter<'_>) -> formatting::Result {
        f.write_str(self)
    }
}
impl formatting::Write for StrTendril {
    fn write_str(&mut self, s: &str) -> formatting::Result {
        self.push_slice(s);
        Ok(())
    }
}
impl From<&str> for StrTendril {
    #[inline]
    fn from(s: &str) -> Self {
        Self::from_slice(s)
    }
}
impl From<String> for StrTendril {
    #[inline]
    fn from(s: String) -> Self {
        if s.len() <= INLINE {
            return Self::from_slice(&s);
        }
        let end = u32::try_from(s.len()).expect("tendril length overflow");
        Self {
            storage: Storage::Heap(Rc::new(s), false.into()),
            start: 0,
            end,
        }
    }
}
impl From<&[u8]> for ByteTendril {
    #[inline]
    fn from(s: &[u8]) -> Self {
        Self::from_slice(s)
    }
}
impl AsRef<str> for StrTendril {
    #[inline]
    fn as_ref(&self) -> &str {
        self
    }
}
impl AsRef<[u8]> for ByteTendril {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self
    }
}
pub trait SliceExt {
    type Output;
    fn to_tendril(&self) -> Self::Output;
}
impl SliceExt for str {
    type Output = StrTendril;
    fn to_tendril(&self) -> StrTendril {
        self.into()
    }
}
impl SliceExt for [u8] {
    type Output = ByteTendril;
    fn to_tendril(&self) -> ByteTendril {
        self.into()
    }
}
impl StrTendril {
    #[inline]
    pub fn from_char(c: char) -> Self {
        Self::from_slice(c.encode_utf8(&mut [0; 4]))
    }
    #[inline]
    pub fn push_char(&mut self, c: char) {
        self.push_slice(c.encode_utf8(&mut [0; 4]));
    }
    #[inline]
    pub fn pop_front_char(&mut self) -> Option<char> {
        let c = self.chars().next()?;
        self.pop_front(c.len_utf8() as u32);
        Some(c)
    }
    pub fn pop_front_char_run<C: FnMut(char) -> R, R: PartialEq>(
        &mut self,
        mut f: C,
    ) -> Option<(Self, R)> {
        let mut it = self.char_indices();
        let class = f(it.next()?.1);
        let end = it
            .find(|(_, c)| f(*c) != class)
            .map(|v| v.0 as u32)
            .unwrap_or(self.len32());
        let run = self.subtendril(0, end);
        self.pop_front(end);
        Some((run, class))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mutation_and_ranges_are_checked() {
        let original = StrTendril::from("🦀abcdefghijé📦0123456789");
        let mut x = original.clone();
        assert_eq!(
            x.try_subtendril(1, 2),
            Err(SubtendrilError::ValidationFailed)
        );
        assert_eq!(
            x.try_subtendril(u32::MAX, 2),
            Err(SubtendrilError::OutOfBounds)
        );
        assert_eq!(x.try_subtendril(1, 0).unwrap().len32(), 0);
        assert_eq!(x.try_pop_front(2), Err(SubtendrilError::ValidationFailed));
        x.pop_front(4);
        x.pop_back(10);
        x.push_char('!');
        assert_eq!(&*x, "abcdefghijé📦!");
        assert_eq!(&*original, "🦀abcdefghijé📦0123456789");
        let mut b = ByteTendril::from_slice(&[0xff; 64]);
        let saved = b.clone();
        b.pop_front(51);
        b.pop_back(5);
        b.push_slice(&[0, 1]);
        assert_eq!(saved.len32(), 64);
        assert_eq!(&b[8..], &[0, 1]);
    }
    #[test]
    fn reserving_a_shared_view_preserves_its_backing() {
        let original = StrTendril::from("aé bcd efgh 🌱 predictable content");
        let mut view = original.subtendril(4, 16);
        let ptr = view.as_ptr();
        view.reserve(0);
        view.reserve(u32::MAX);
        assert_eq!(view.as_ptr(), ptr);
        assert_eq!(ptr, original[4..].as_ptr());
        view.push_char('!');
        assert_eq!(&*view, format!("{}!", &original[4..20]));
    }

    #[test]
    fn adjacent_ranges_rejoin_without_copying() {
        let t = StrTendril::from("abcdefghijklmnopqrstuvwxyz0123456789");
        let mut a = t.subtendril(0, 18);
        let b = t.subtendril(18, 18);
        a.push_tendril(&b);
        assert_eq!(a, t);
        assert_eq!(a.as_ptr(), t.as_ptr());
        let middle = t.subtendril(2, 10);
        a.push_tendril(&middle);
        assert_eq!(&a[36..], &*middle);
        assert_eq!(t.len32(), 36);
    }
    #[test]
    fn consume_append_window_does_not_copy_each_step_or_pin_consumed_input() {
        let mut v = StrTendril::from("x".repeat(8192));
        let mut rebases = 0;
        for _ in 0..32768 {
            v.pop_front(1);
            let previous = v.start;
            v.push_char('x');
            rebases += usize::from(v.start < previous);
            assert!(v.start <= 8192);
        }
        assert!((3..=5).contains(&rebases));
        assert_eq!(v.len32(), 8192);
        v.pop_front(8190);
        assert!(matches!(v.storage, Storage::Inline(_)));
        assert_eq!(&*v, "xx");
    }
    #[test]
    fn writing_chunks_and_reading_short_or_interrupted_streams() {
        use std::io::{self, Read};
        struct Split {
            at: usize,
            interrupted: bool,
        }
        impl Read for Split {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let data = "ó🦀";
                if self.at == data.len() {
                    return Ok(0);
                }
                b[0] = data.as_bytes()[self.at];
                self.at += 1;
                Ok(1)
            }
        }
        struct Sink(String);
        impl TendrilSink<fmt::UTF8> for Sink {
            type Output = String;
            fn process(&mut self, t: StrTendril) {
                self.0.push_str(&t)
            }
            fn error(&mut self, _: std::borrow::Cow<'static, str>) {
                panic!("unexpected decode error")
            }
            fn finish(self) -> String {
                self.0
            }
        }
        let x = stream::Utf8LossyDecoder::new(Sink(String::new()))
            .read_from(&mut Split {
                at: 0,
                interrupted: false,
            })
            .unwrap();
        assert_eq!(x, "ó🦀");
    }
}
