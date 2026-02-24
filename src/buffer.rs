//! Chunk buffering for streaming body processing.

use crate::errors::ImageOptError;

/// Buffer for accumulating body chunks.
#[derive(Debug)]
pub struct ChunkBuffer {
    /// Accumulated data.
    data: Vec<u8>,
    /// Maximum buffer size.
    max_size: usize,
}

impl ChunkBuffer {
    /// Create a new chunk buffer with the specified maximum size.
    pub fn new(max_size: usize) -> Self {
        Self {
            data: Vec::new(),
            max_size,
        }
    }

    /// Append a chunk to the buffer.
    ///
    /// # Errors
    ///
    /// Returns `ImageOptError::BufferOverflow` if the buffer would exceed the maximum size.
    pub fn append(&mut self, chunk: &[u8]) -> Result<(), ImageOptError> {
        if self.data.len() + chunk.len() > self.max_size {
            return Err(ImageOptError::BufferOverflow {
                max_bytes: self.max_size,
            });
        }
        self.data.extend_from_slice(chunk);
        Ok(())
    }

    /// Take the accumulated data and reset the buffer.
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.data)
    }

    /// Get the current size of buffered data.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Clear the buffer.
    pub fn clear(&mut self) {
        self.data.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_accumulates_data() {
        let mut buffer = ChunkBuffer::new(100);
        assert!(buffer.append(b"hello").is_ok());
        assert!(buffer.append(b" world").is_ok());
        assert_eq!(buffer.len(), 11);
    }

    #[test]
    fn append_rejects_overflow() {
        let mut buffer = ChunkBuffer::new(10);
        assert!(buffer.append(b"hello").is_ok());
        let result = buffer.append(b"world!");
        assert!(matches!(result, Err(ImageOptError::BufferOverflow { .. })));
    }

    #[test]
    fn take_returns_data_and_resets() {
        let mut buffer = ChunkBuffer::new(100);
        buffer.append(b"test data").unwrap();
        let data = buffer.take();
        assert_eq!(data, b"test data");
        assert!(buffer.is_empty());
    }

    #[test]
    fn empty_buffer_reports_correctly() {
        let buffer = ChunkBuffer::new(100);
        assert!(buffer.is_empty());
        assert_eq!(buffer.len(), 0);
    }
}
