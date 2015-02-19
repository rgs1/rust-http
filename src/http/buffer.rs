/// Memory buffers for the benefit of `std::old_io::net` which has slow read/write.

use std::old_io::{IoResult, Stream};
use std::cmp::min;
use std::slice;
use std::fmt::radix;
use std::ptr;

// 64KB chunks (moderately arbitrary)
const READ_BUF_SIZE: usize = 0x10000;
const WRITE_BUF_SIZE: usize = 0x10000;
// TODO: consider removing constants and giving a buffer size in the constructor

pub struct BufferedStream<T> {
    pub wrapped: T,
    pub read_buffer: Vec<u8>,
    // The current position in the buffer
    pub read_pos: usize,
    // The last valid position in the reader
    pub read_max: usize,
    pub write_buffer: Vec<u8>,
    pub write_len: usize,

    pub writing_chunked_body: bool,
}

impl<T: Stream> BufferedStream<T> {
    pub fn new(stream: T) -> BufferedStream<T> {
        let mut read_buffer = Vec::with_capacity(READ_BUF_SIZE);
        unsafe { read_buffer.set_len(READ_BUF_SIZE); }
        let mut write_buffer = Vec::with_capacity(WRITE_BUF_SIZE);
        unsafe { write_buffer.set_len(WRITE_BUF_SIZE); }
        BufferedStream {
            wrapped: stream,
            read_buffer: read_buffer,
            read_pos: 0us,
            read_max: 0us,
            write_buffer: write_buffer,
            write_len: 0us,
            writing_chunked_body: false,
        }
    }
}

impl<T: Reader> BufferedStream<T> {
    /// Poke a single byte back so it will be read next. For this to make sense, you must have just
    /// read that byte. If `self.pos` is 0 and `self.max` is not 0 (i.e. if the buffer is just
    /// filled
    /// Very great caution must be used in calling this as it will fail if `self.pos` is 0.
    pub fn poke_byte(&mut self, byte: u8) {
        match (self.read_pos, self.read_max) {
            (0, 0) => self.read_max = 1,
            (0, _) => panic!("poke called when buffer is full"),
            (_, _) => self.read_pos -= 1,
        }
        self.read_buffer[self.read_pos] = byte;
    }

    #[inline]
    fn fill_buffer(&mut self) -> IoResult<()> {
        assert_eq!(self.read_pos, self.read_max);
        self.read_pos = 0;
        match self.wrapped.read(self.read_buffer.as_mut_slice()) {
            Ok(i) => {
                self.read_max = i;
                Ok(())
            },
            Err(err) => {
                self.read_max = 0;
                Err(err)
            },
        }
    }

    /// Slightly faster implementation of read_byte than that which is provided by ReaderUtil
    /// (which just uses `read()`)
    #[inline]
    pub fn read_byte(&mut self) -> IoResult<u8> {
        if self.read_pos == self.read_max {
            // Fill the buffer, giving up if we've run out of buffered content
            try!(self.fill_buffer());
        }
        self.read_pos += 1;
        Ok(self.read_buffer[self.read_pos - 1])
    }
}

impl<T: Writer> BufferedStream<T> {
    /// Finish off writing a response: this flushes the writer and in case of chunked
    /// Transfer-Encoding writes the ending zero-length chunk to indicate completion.
    ///
    /// At the time of calling this, headers MUST have been written, including the
    /// ending CRLF, or else an invalid HTTP response may be written.
    pub fn finish_response(&mut self) -> IoResult<()> {
        try!(self.flush());
        if self.writing_chunked_body {
            try!(self.wrapped.write_all(b"0\r\n\r\n"));
        }
        Ok(())
    }
}

impl<T: Reader> Reader for BufferedStream<T> {
    /// Read at most N bytes into `buf`, where N is the minimum of `buf.len()` and the buffer size.
    ///
    /// At present, this makes no attempt to fill its buffer proactively, instead waiting until you
    /// ask.
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        if self.read_pos == self.read_max {
            // Fill the buffer, giving up if we've run out of buffered content
            try!(self.fill_buffer());
        }
        let size = min(self.read_max - self.read_pos, buf.len());
        slice::bytes::copy_memory(buf, &self.read_buffer[self.read_pos..self.read_pos + size]);
        self.read_pos += size;
        Ok(size)
    }
}

impl<T: Writer> Writer for BufferedStream<T> {
    fn write_all(&mut self, buf: &[u8]) -> IoResult<()> {
        if buf.len() + self.write_len > self.write_buffer.len() {
            // This is the lazy approach which may involve multiple writes where it's really not
            // warranted. Maybe deal with that later.
            if self.writing_chunked_body {
                let s = format!("{}\r\n", (radix(self.write_len + buf.len(), 16)));
                try!(self.wrapped.write_all(s.as_bytes()));
            }
            if self.write_len > 0 {
                try!(self.wrapped.write_all(&self.write_buffer[..self.write_len]));
                self.write_len = 0;
            }
            try!(self.wrapped.write_all(buf));
            self.write_len = 0;
            if self.writing_chunked_body {
                try!(self.wrapped.write_all(b"\r\n"));
            }
        } else {
            unsafe {
                ptr::copy_memory(self.write_buffer.as_mut_ptr().offset(self.write_len as isize),
                    buf.as_ptr(), buf.len());
            }

            self.write_len += buf.len();
            if self.write_len == self.write_buffer.len() {
                if self.writing_chunked_body {
                    let s = format!("{}\r\n", radix(self.write_len, 16));
                    try!(self.wrapped.write_all(s.as_bytes()));
                    try!(self.wrapped.write_all(&self.write_buffer[]));
                    try!(self.wrapped.write_all(b"\r\n"));
                } else {
                    try!(self.wrapped.write_all(&self.write_buffer[]));
                }
                self.write_len = 0;
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> IoResult<()> {
        if self.write_len > 0 {
            if self.writing_chunked_body {
                let s = format!("{}\r\n", radix(self.write_len, 16));
                try!(self.wrapped.write_all(s.as_bytes()));
            }
            try!(self.wrapped.write_all(&self.write_buffer[..self.write_len]));
            if self.writing_chunked_body {
                try!(self.wrapped.write_all(b"\r\n"));
            }
            self.write_len = 0;
        }
        self.wrapped.flush()
    }
}
