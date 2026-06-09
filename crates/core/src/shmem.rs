//! Shared Memory IPC Ring Buffer for high-performance zero-copy event routing.

use crate::events::Event;
use memmap2::{Mmap, MmapMut};
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, warn};

// Mathematical alignments for raw shared-memory offsets
const HEADER_SIZE: usize = 32;
const SLOT_HEADER_SIZE: usize = 8; // 4 bytes status + 4 bytes length
const SLOT_PAYLOAD_SIZE: usize = 4096; // 4KB slots
const SLOT_SIZE: usize = SLOT_HEADER_SIZE + SLOT_PAYLOAD_SIZE; // 4104 bytes
const SLOT_COUNT: usize = 1024;
pub const TOTAL_SHMEM_SIZE: usize = HEADER_SIZE + SLOT_SIZE * SLOT_COUNT; // ~4.2 MB

const MAGIC_BYTES: u32 = 0x4D455243; // "MERC" in hex

/// Shmem Server that writes events to the memory-mapped file ring buffer.
pub struct ShmemServer {
    _file_path: PathBuf,
    mmap: MmapMut,
}

impl ShmemServer {
    /// Create or open the shared memory mapped file.
    pub fn new(file_path: PathBuf) -> std::io::Result<Self> {
        if file_path.exists() {
            let _ = std::fs::remove_file(&file_path);
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&file_path)?;

        file.set_len(TOTAL_SHMEM_SIZE as u64)?;
        let mut mmap = unsafe { MmapMut::map_mut(&file)? };

        // Initialize Global Header
        unsafe {
            write_u32(mmap.as_mut_ptr(), 0, MAGIC_BYTES);
            write_u32(mmap.as_mut_ptr(), 4, SLOT_COUNT as u32);
            write_u64_atomic(mmap.as_mut_ptr(), 8, 0);
        }

        // Initialize all slots to free status (0)
        for i in 0..SLOT_COUNT {
            let slot_offset = HEADER_SIZE + i * SLOT_SIZE;
            unsafe {
                write_u32(mmap.as_mut_ptr(), slot_offset, 0);
                write_u32(mmap.as_mut_ptr(), slot_offset + 4, 0);
            }
        }

        info!("Shared-Memory IPC Server initialized at {:?}", file_path);
        Ok(Self {
            _file_path: file_path,
            mmap,
        })
    }

    /// Write and broadcast an event to the shared memory mapped ring buffer slot.
    pub fn broadcast(&mut self, event: &Event) -> Result<(), &'static str> {
        let json_str = serde_json::to_string(event).map_err(|_| "Failed to serialize event")?;
        let bytes = json_str.as_bytes();

        if bytes.len() > SLOT_PAYLOAD_SIZE {
            warn!("Event size {} exceeds Shmem slot size {}", bytes.len(), SLOT_PAYLOAD_SIZE);
            return Err("Event too large for slot");
        }

        unsafe {
            let write_cursor = read_u64_atomic(self.mmap.as_ptr(), 8);
            let slot_idx = (write_cursor % SLOT_COUNT as u64) as usize;
            let slot_offset = HEADER_SIZE + slot_idx * SLOT_SIZE;

            // Mark slot status as 1 (writing)
            write_u32(self.mmap.as_mut_ptr(), slot_offset, 1);

            // Copy payload bytes into mapped memory
            let payload_ptr = self.mmap.as_mut_ptr().add(slot_offset + SLOT_HEADER_SIZE);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), payload_ptr, bytes.len());

            // Write payload length
            write_u32(self.mmap.as_mut_ptr(), slot_offset + 4, bytes.len() as u32);

            // Mark slot status as 2 (ready)
            write_u32(self.mmap.as_mut_ptr(), slot_offset, 2);

            // Monotonically advance writer cursor using atomic release barrier
            write_u64_atomic(self.mmap.as_mut_ptr(), 8, write_cursor + 1);
        }

        Ok(())
    }
}

/// Shmem Client that reads events from the memory-mapped file ring buffer.
pub struct ShmemClient {
    _file_path: PathBuf,
    mmap: Mmap,
    read_cursor: u64,
}

impl ShmemClient {
    /// Connect to a running shared-memory mapped file.
    pub fn connect(file_path: PathBuf) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .open(&file_path)?;

        let mmap = unsafe { Mmap::map(&file)? };

        // Verify Magic Identifier
        let magic = unsafe { read_u32(mmap.as_ptr(), 0) };
        if magic != MAGIC_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid shared memory magic identifier",
            ));
        }

        // Initialize read_cursor to current write_cursor to skip stale historical ticks
        let initial_cursor = unsafe { read_u64_atomic(mmap.as_ptr(), 8) };

        info!("Shared-Memory IPC Client connected successfully to {:?}", file_path);
        Ok(Self {
            _file_path: file_path,
            mmap,
            read_cursor: initial_cursor,
        })
    }

    /// Read next event from shared-memory ring buffer (non-blocking).
    pub fn try_recv(&mut self) -> Option<Event> {
        unsafe {
            let write_cursor = read_u64_atomic(self.mmap.as_ptr(), 8);

            if self.read_cursor >= write_cursor {
                return None; // No new events available
            }

            // Reader lagged protection (if writer advanced beyond one complete ring rotation)
            if write_cursor - self.read_cursor > SLOT_COUNT as u64 {
                warn!(
                    "Shmem reader lagged: skipping forward from {} to {}",
                    self.read_cursor,
                    write_cursor - SLOT_COUNT as u64
                );
                self.read_cursor = write_cursor - SLOT_COUNT as u64;
            }

            let slot_idx = (self.read_cursor % SLOT_COUNT as u64) as usize;
            let slot_offset = HEADER_SIZE + slot_idx * SLOT_SIZE;

            let status = read_u32(self.mmap.as_ptr(), slot_offset);
            if status == 2 {
                // Read payload length
                let length = read_u32(self.mmap.as_ptr(), slot_offset + 4) as usize;
                
                // Read slice safely
                let data_ptr = self.mmap.as_ptr().add(slot_offset + SLOT_HEADER_SIZE);
                let bytes = std::slice::from_raw_parts(data_ptr, length);

                // Increment cursor
                self.read_cursor += 1;

                if let Ok(event) = serde_json::from_slice::<Event>(bytes) {
                    return Some(event);
                }
            }

            None
        }
    }
}

// Low-level helper pointers operations using raw volatile mappings
#[inline(always)]
unsafe fn write_u32(ptr: *mut u8, offset: usize, val: u32) {
    unsafe {
        let target = ptr.add(offset) as *mut u32;
        std::ptr::write_volatile(target, val);
    }
}

#[inline(always)]
unsafe fn read_u32(ptr: *const u8, offset: usize) -> u32 {
    unsafe {
        let target = ptr.add(offset) as *const u32;
        std::ptr::read_volatile(target)
    }
}

#[inline(always)]
unsafe fn write_u64_atomic(ptr: *mut u8, offset: usize, val: u64) {
    unsafe {
        let target = ptr.add(offset) as *const AtomicU64;
        (*target).store(val, Ordering::Release);
    }
}

#[inline(always)]
unsafe fn read_u64_atomic(ptr: *const u8, offset: usize) -> u64 {
    unsafe {
        let target = ptr.add(offset) as *const AtomicU64;
        (*target).load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventPayload, RiskAlert, RiskAlertType};
    use std::time::Duration;

    #[test]
    fn test_shmem_ipc_lifecycle() {
        let test_id = crate::types::now_nanos();
        let shmem_path = std::env::temp_dir().join(format!("mercury_shmem_test_{}.bin", test_id));

        // Start server
        let mut server = ShmemServer::new(shmem_path.clone()).expect("Failed to start Shmem server");

        // Connect client
        let mut client = ShmemClient::connect(shmem_path.clone()).expect("Failed to connect Shmem client");

        // Broadcast a dummy event
        let event = Event::new(
            88,
            EventPayload::RiskAlert(RiskAlert {
                alert_type: RiskAlertType::RateLimitExceeded,
                message: "Shmem limit warning test".to_string(),
                timestamp: crate::types::now_nanos(),
            }),
        );

        server.broadcast(&event).expect("Failed to broadcast shmem event");

        // Sleep briefly to yield thread execution
        std::thread::sleep(Duration::from_millis(5));

        // Read event from client
        let received_option = client.try_recv();
        assert!(received_option.is_some(), "Client failed to read event from Shmem");

        let received_event = received_option.unwrap();
        assert_eq!(received_event.id, 88);

        match received_event.payload {
            EventPayload::RiskAlert(alert) => {
                assert_eq!(alert.message, "Shmem limit warning test");
            }
            _ => panic!("Expected RiskAlert payload"),
        }

        // Clean up
        if shmem_path.exists() {
            let _ = std::fs::remove_file(&shmem_path);
        }
    }
}
