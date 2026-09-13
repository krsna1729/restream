//! Generation-safe fixed file table for one owner thread.

use std::io;
use std::os::fd::RawFd;

use io_uring::{Submitter, types};

use crate::MAX_TAG_SLOTS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedFile {
    pub index: u32,
    pub generation: u32,
}

#[derive(Debug)]
pub struct FixedFileTable {
    files: Box<[RawFd]>,
    generations: Box<[u32]>,
    free: Vec<u32>,
}

impl FixedFileTable {
    pub fn new(capacity: usize) -> io::Result<Self> {
        if capacity == 0 || capacity >= MAX_TAG_SLOTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid fixed file table capacity",
            ));
        }
        let mut free = Vec::with_capacity(capacity);
        for index in (0..capacity as u32).rev() {
            free.push(index);
        }
        Ok(Self {
            files: vec![-1; capacity].into_boxed_slice(),
            generations: vec![0; capacity].into_boxed_slice(),
            free,
        })
    }

    /// Registers sparse slots with the ring. Call once before `install`.
    pub fn register(&self, submitter: &Submitter) -> io::Result<()> {
        submitter.register_files(&self.files)
    }

    pub fn install(&mut self, submitter: &Submitter, fd: RawFd) -> io::Result<FixedFile> {
        let index = self
            .free
            .pop()
            .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, "fixed file table full"))?;
        if let Err(error) = update(submitter, index, fd) {
            self.free.push(index);
            return Err(error);
        }
        self.files[index as usize] = fd;
        Ok(FixedFile {
            index,
            generation: self.generations[index as usize],
        })
    }

    pub fn remove(&mut self, submitter: &Submitter, file: FixedFile) -> io::Result<bool> {
        let Some(current) = self.generations.get(file.index as usize) else {
            return Ok(false);
        };
        if *current != file.generation || self.files[file.index as usize] < 0 {
            return Ok(false);
        }
        update(submitter, file.index, -1)?;
        self.files[file.index as usize] = -1;
        self.generations[file.index as usize] = file.generation.wrapping_add(1);
        self.free.push(file.index);
        Ok(true)
    }

    pub fn get(&self, file: FixedFile) -> Option<types::Fixed> {
        (self.generations.get(file.index as usize) == Some(&file.generation)
            && self.files[file.index as usize] >= 0)
            .then_some(types::Fixed(file.index))
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn available(&self) -> usize {
        self.free.len()
    }
}

fn update(submitter: &Submitter, index: u32, fd: RawFd) -> io::Result<()> {
    let updated = submitter.register_files_update(index, &[fd])?;
    if updated == 1 {
        Ok(())
    } else {
        Err(io::Error::other(
            "fixed file update did not update one slot",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_generation_safe_without_a_ring() {
        let mut table = FixedFileTable::new(2).unwrap();
        let first = table.install_without_kernel_for_test(7).unwrap();
        assert_eq!(table.get(first).map(|file| file.0), Some(first.index));
        let removed = table.remove_without_kernel_for_test(first).unwrap();
        assert!(removed);
        assert!(table.get(first).is_none());
        let second = table.install_without_kernel_for_test(8).unwrap();
        assert_ne!(first.generation, second.generation);
        assert_eq!(table.get(second).map(|file| file.0), Some(second.index));
    }

    impl FixedFileTable {
        fn install_without_kernel_for_test(&mut self, fd: RawFd) -> Option<FixedFile> {
            let index = self.free.pop()?;
            self.files[index as usize] = fd;
            Some(FixedFile {
                index,
                generation: self.generations[index as usize],
            })
        }

        fn remove_without_kernel_for_test(&mut self, file: FixedFile) -> Option<bool> {
            if self.get(file).is_none() {
                return Some(false);
            }
            self.files[file.index as usize] = -1;
            self.generations[file.index as usize] = file.generation.wrapping_add(1);
            self.free.push(file.index);
            Some(true)
        }
    }
}
