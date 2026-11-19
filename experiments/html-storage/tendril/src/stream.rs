// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{ByteTendril, Format, StrTendril, Tendril, fmt};
use std::{
    borrow::Cow,
    io::{self, Read},
    path::Path,
};
pub trait TendrilSink<F: Format> {
    fn process(&mut self, t: Tendril<F>);
    fn error(&mut self, desc: Cow<'static, str>);
    type Output;
    fn finish(self) -> Self::Output;
    fn one<T: Into<Tendril<F>>>(mut self, t: T) -> Self::Output
    where
        Self: Sized,
    {
        self.process(t.into());
        self.finish()
    }
    #[allow(
        clippy::wrong_self_convention,
        reason = "Match the published sink interface."
    )]
    fn from_iter<I: IntoIterator>(mut self, it: I) -> Self::Output
    where
        Self: Sized,
        I::Item: Into<Tendril<F>>,
    {
        for t in it {
            self.process(t.into());
        }
        self.finish()
    }
    fn read_from<R: Read>(mut self, r: &mut R) -> io::Result<Self::Output>
    where
        Self: Sized,
        F: Format<Slice = [u8]>,
    {
        let mut b = [0u8; 4096];
        loop {
            match r.read(&mut b) {
                Ok(0) => return Ok(self.finish()),
                Ok(n) => self.process(Tendril::from_slice(&b[..n])),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
    #[allow(
        clippy::wrong_self_convention,
        reason = "Match the published sink interface."
    )]
    fn from_file<P: AsRef<Path>>(self, path: P) -> io::Result<Self::Output>
    where
        Self: Sized,
        F: Format<Slice = [u8]>,
    {
        self.read_from(&mut std::fs::File::open(path)?)
    }
}
pub struct Utf8LossyDecoder<S> {
    pub inner_sink: S,
    carry: [u8; 4],
    used: usize,
}
impl<S: TendrilSink<fmt::UTF8>> Utf8LossyDecoder<S> {
    pub fn new(inner_sink: S) -> Self {
        Self {
            inner_sink,
            carry: [0; 4],
            used: 0,
        }
    }
    fn invalid(&mut self) {
        self.inner_sink.error("invalid byte sequence".into());
        self.inner_sink.process(StrTendril::from_char('\u{fffd}'))
    }
}
impl<S: TendrilSink<fmt::UTF8>> TendrilSink<fmt::Bytes> for Utf8LossyDecoder<S> {
    type Output = S::Output;
    fn process(&mut self, input: ByteTendril) {
        let mut bytes = &input[..];
        if self.used != 0 {
            let expected = match self.carry[0] {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => unreachable!(),
            };
            let take = bytes.len().min(expected - self.used);
            self.carry[self.used..self.used + take].copy_from_slice(&bytes[..take]);
            match std::str::from_utf8(&self.carry[..self.used + take]) {
                Ok(text) => {
                    self.inner_sink.process(StrTendril::from_slice(text));
                    bytes = &bytes[take..];
                }
                Err(e) => match e.error_len() {
                    Some(n) => {
                        let consumed = n - self.used;
                        self.invalid();
                        bytes = &bytes[consumed..];
                    }
                    None => {
                        self.used += take;
                        return;
                    }
                },
            }
            self.used = 0;
        }
        while !bytes.is_empty() {
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    self.inner_sink.process(StrTendril::from_slice(text));
                    return;
                }
                Err(e) => {
                    let pos = e.valid_up_to();
                    if pos > 0 {
                        self.inner_sink.process(StrTendril::from_slice(
                            std::str::from_utf8(&bytes[..pos]).unwrap(),
                        ));
                    }
                    bytes = &bytes[pos..];
                    if let Some(n) = e.error_len() {
                        self.invalid();
                        bytes = &bytes[n..];
                    } else {
                        self.used = bytes.len();
                        self.carry[..self.used].copy_from_slice(bytes);
                        return;
                    }
                }
            }
        }
    }
    fn error(&mut self, desc: Cow<'static, str>) {
        self.inner_sink.error(desc)
    }
    fn finish(mut self) -> S::Output {
        if self.used != 0 {
            self.inner_sink
                .error("incomplete byte sequence at end of stream".into());
            self.inner_sink.process(StrTendril::from_char('\u{fffd}'));
        }
        self.inner_sink.finish()
    }
}
