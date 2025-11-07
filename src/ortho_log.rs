use crate::ortho::Ortho;
use crate::FoldError;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::PathBuf;

/// Utilities for reading orthos from a persistent disk-backed file
/// Uses the same format as DiskQueue for compatibility
pub struct OrthoStorage;

impl OrthoStorage {
    /// Get the path to the persistent ortho storage file
    pub fn storage_path() -> PathBuf {
        let state_dir = std::env::var("FOLD_STATE_DIR")
            .unwrap_or_else(|_| "./fold_state".to_string());
        PathBuf::from(&state_dir).join("ortho_storage.bin")
    }

    /// Read all orthos from persistent storage
    pub fn read_all() -> Result<Vec<Ortho>, FoldError> {
        let path = Self::storage_path();
        
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&path).map_err(|e| FoldError::Io(e))?;
        let mut reader = BufReader::new(file);
        let mut orthos = Vec::new();

        loop {
            // Read length prefix
            let mut len_bytes = [0u8; 4];
            match reader.read_exact(&mut len_bytes) {
                Ok(_) => {},
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(FoldError::Io(e)),
            }
            let len = u32::from_le_bytes(len_bytes) as usize;

            // Read data
            let mut buffer = vec![0u8; len];
            reader.read_exact(&mut buffer)
                .map_err(|e| FoldError::Io(e))?;

            // Deserialize
            let (ortho, _): (Ortho, _) = bincode::decode_from_slice(&buffer, bincode::config::standard())
                .map_err(|e| FoldError::Deserialization(Box::new(e)))?;

            orthos.push(ortho);
        }

        println!("[OrthoStorage] Read {} orthos from disk", orthos.len());
        Ok(orthos)
    }

    /// Count orthos in storage without loading them all
    pub fn count() -> Result<usize, FoldError> {
        let path = Self::storage_path();
        
        if !path.exists() {
            return Ok(0);
        }

        let file = File::open(&path).map_err(|e| FoldError::Io(e))?;
        let mut reader = BufReader::new(file);
        let mut count = 0;

        loop {
            // Read length prefix
            let mut len_bytes = [0u8; 4];
            match reader.read_exact(&mut len_bytes) {
                Ok(_) => {},
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(FoldError::Io(e)),
            }
            let len = u32::from_le_bytes(len_bytes) as usize;

            // Skip the data
            let mut buffer = vec![0u8; len];
            reader.read_exact(&mut buffer)
                .map_err(|e| FoldError::Io(e))?;

            count += 1;
        }

        Ok(count)
    }
}
