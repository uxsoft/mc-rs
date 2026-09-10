//! Bounded, memory-only seeking for sequential archive members. No plaintext files.
use super::*;
use std::io::SeekFrom;
pub const LIMIT: u64 = 64 * 1024 * 1024;
pub struct SeekCache {
    input: Box<dyn Read + Send>,
    bytes: zeroize::Zeroizing<Vec<u8>>,
    size: u64,
    position: u64,
    ctx: Context,
}
impl SeekCache {
    pub fn new(input: Box<dyn Read + Send>, size: u64, ctx: Context) -> Result<Self> {
        anyhow::ensure!(
            size <= LIMIT,
            "Archive seek cache limit is 64 MiB per member; copy this archive locally to open it"
        );
        Ok(Self {
            input,
            bytes: zeroize::Zeroizing::new(Vec::with_capacity(size as usize)),
            size,
            position: 0,
            ctx,
        })
    }
}
impl Read for SeekCache {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.ctx.check().map_err(io::Error::other)?;
        let n = (self.size.saturating_sub(self.position)).min(out.len() as u64) as usize;
        if n == 0 {
            return Ok(0);
        }
        let end = self.position as usize + n;
        let mut chunk = [0; 65536];
        while self.bytes.len() < end {
            self.ctx.check().map_err(io::Error::other)?;
            let need = (end - self.bytes.len()).min(chunk.len());
            let n = self.input.read(&mut chunk[..need])?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Archive member ended before its declared size",
                ));
            }
            self.bytes.extend_from_slice(&chunk[..n]);
        }
        out[..n].copy_from_slice(&self.bytes[self.position as usize..end]);
        self.position += n as u64;
        Ok(n)
    }
}
impl Seek for SeekCache {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.ctx.check().map_err(io::Error::other)?;
        let next = match pos {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            SeekFrom::End(n) => i128::from(self.size) + i128::from(n),
        };
        if next < 0 || next > i128::from(LIMIT) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Seek outside bounded archive cache",
            ));
        }
        self.position = next as u64;
        Ok(self.position)
    }
}
