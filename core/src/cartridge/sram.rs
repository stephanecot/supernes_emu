//! Battery-backed SRAM byte buffer. Persistence to .srm files is the
//! frontend's job (core has no I/O).

pub struct Sram {
    data: Vec<u8>,
}

impl Sram {
    pub fn new(size: usize) -> Self {
        Sram { data: vec![0xFF; size] }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Read with mirroring across the SRAM size. `None` when no SRAM is present.
    pub fn get(&self, offset: usize) -> Option<u8> {
        if self.data.is_empty() {
            None
        } else {
            Some(self.data[offset % self.data.len()])
        }
    }

    pub fn set(&mut self, offset: usize, value: u8) {
        if !self.data.is_empty() {
            let len = self.data.len();
            self.data[offset % len] = value;
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Load previously saved contents (frontend restores .srm files with this).
    pub fn load(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.data.len());
        self.data[..n].copy_from_slice(&bytes[..n]);
    }
}
