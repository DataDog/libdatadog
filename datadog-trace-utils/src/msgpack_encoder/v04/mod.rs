use rmp::encode::{write_array_len, ByteBuf, RmpWrite, ValueWriteError};
use crate::span::{Span, SpanText};

mod span;

#[inline(always)]
fn to_writer<W: RmpWrite, T: SpanText>(writer: &mut W, traces: &Vec<Vec<Span<T>>>) -> Result<(), ValueWriteError<W::Error>> {
    write_array_len(writer, traces.len() as u32)?;
    for trace in traces {
        write_array_len(writer, trace.len() as u32)?;
        for span in trace {
            span::encode_span(writer, &span)?;
        }
    }

    Ok(())
}

pub fn to_slice<T: SpanText>(mut slice: &mut [u8], traces: &Vec<Vec<Span<T>>>) -> Result<(), ValueWriteError> {
    to_writer(&mut slice, traces)
}

pub fn to_vec<T: SpanText>(traces: &Vec<Vec<Span<T>>>) -> Vec<u8> {
    to_vec_with_capacity(traces, 0)
}

pub fn to_vec_with_capacity<T: SpanText>(traces: &Vec<Vec<Span<T>>>, capacity: u32) -> Vec<u8> {
    let mut buf = ByteBuf::with_capacity(capacity as usize);
    let _ = to_writer(&mut buf, traces);
    buf.into_vec()
}

struct CountLength(u32);

impl std::io::Write for CountLength {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_all(buf)?;
        Ok(buf.len())
    }

    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.0 += buf.len() as u32;
        Ok(())
    }
}

pub fn to_len<T: SpanText>(traces: &Vec<Vec<Span<T>>>) -> u32 {
    let mut counter = CountLength(0);
    let _ = to_writer(&mut counter, traces);
    counter.0
}
