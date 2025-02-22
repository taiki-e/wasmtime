use crate::Mmap;
use anyhow::{Context, Result};
use std::fs::File;
use std::ops::{Deref, DerefMut, Range};
use std::path::Path;
use std::sync::Arc;

/// A type akin to `Vec<u8>`, but backed by `mmap` and able to be split.
///
/// This type is a non-growable owned list of bytes. It can be segmented into
/// disjoint separately owned views akin to the `split_at` method on slices in
/// Rust. An `MmapVec` is backed by an OS-level memory allocation and is not
/// suitable for lots of small allocation (since it works at the page
/// granularity).
///
/// An `MmapVec` is an owned value which means that owners have the ability to
/// get exclusive access to the underlying bytes, enabling mutation.
pub struct MmapVec {
    mmap: Arc<Mmap>,
    range: Range<usize>,
}

impl MmapVec {
    /// Consumes an existing `mmap` and wraps it up into an `MmapVec`.
    ///
    /// The returned `MmapVec` will have the `size` specified, which can be
    /// smaller than the region mapped by the `Mmap`. The returned `MmapVec`
    /// will only have at most `size` bytes accessible.
    pub fn new(mmap: Mmap, size: usize) -> MmapVec {
        assert!(size <= mmap.len());
        MmapVec {
            mmap: Arc::new(mmap),
            range: 0..size,
        }
    }

    /// Creates a new zero-initialized `MmapVec` with the given `size`.
    ///
    /// This commit will return a new `MmapVec` suitably sized to hold `size`
    /// bytes. All bytes will be initialized to zero since this is a fresh OS
    /// page allocation.
    pub fn with_capacity(size: usize) -> Result<MmapVec> {
        Ok(MmapVec::new(Mmap::with_at_least(size)?, size))
    }

    /// Creates a new `MmapVec` from the contents of an existing `slice`.
    ///
    /// A new `MmapVec` is allocated to hold the contents of `slice` and then
    /// `slice` is copied into the new mmap. It's recommended to avoid this
    /// method if possible to avoid the need to copy data around.
    pub fn from_slice(slice: &[u8]) -> Result<MmapVec> {
        let mut result = MmapVec::with_capacity(slice.len())?;
        result.copy_from_slice(slice);
        Ok(result)
    }

    /// Creates a new `MmapVec` which is the `path` specified mmap'd into
    /// memory.
    ///
    /// This function will attempt to open the file located at `path` and will
    /// then use that file to learn about its size and map the full contents
    /// into memory. This will return an error if the file doesn't exist or if
    /// it's too large to be fully mapped into memory.
    pub fn from_file(path: &Path) -> Result<MmapVec> {
        let mmap = Mmap::from_file(path)
            .with_context(|| format!("failed to create mmap for file: {}", path.display()))?;
        let len = mmap.len();
        Ok(MmapVec::new(mmap, len))
    }

    /// Returns whether the original mmap was created from a readonly mapping.
    pub fn is_readonly(&self) -> bool {
        self.mmap.is_readonly()
    }

    /// Splits the collection into two at the given index.
    ///
    /// Returns a separate `MmapVec` which shares the underlying mapping, but
    /// only has access to elements in the range `[at, len)`. After the call,
    /// the original `MmapVec` will be left with access to the elements in the
    /// range `[0, at)`.
    ///
    /// This is an `O(1)` operation which does not involve copies.
    pub fn split_off(&mut self, at: usize) -> MmapVec {
        assert!(at <= self.range.len());

        // Create a new `MmapVec` which refers to the same underlying mmap, but
        // has a disjoint range from ours. Our own range is adjusted to be
        // disjoint just after `ret` is created.
        let ret = MmapVec {
            mmap: self.mmap.clone(),
            range: at..self.range.end,
        };
        self.range.end = self.range.start + at;
        return ret;
    }

    /// Makes the specified `range` within this `mmap` to be read/write.
    pub unsafe fn make_writable(&self, range: Range<usize>) -> Result<()> {
        self.mmap
            .make_writable(range.start + self.range.start..range.end + self.range.start)
    }

    /// Makes the specified `range` within this `mmap` to be read/execute.
    pub unsafe fn make_executable(&self, range: Range<usize>) -> Result<()> {
        self.mmap
            .make_executable(range.start + self.range.start..range.end + self.range.start)
    }

    /// Returns the underlying file that this mmap is mapping, if present.
    pub fn original_file(&self) -> Option<&Arc<File>> {
        self.mmap.original_file()
    }

    /// Returns the offset within the original mmap that this `MmapVec` is
    /// created from.
    pub fn original_offset(&self) -> usize {
        self.range.start
    }
}

impl Deref for MmapVec {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.mmap.as_slice()[self.range.clone()]
    }
}

impl DerefMut for MmapVec {
    fn deref_mut(&mut self) -> &mut [u8] {
        debug_assert!(!self.is_readonly());
        // SAFETY: The underlying mmap is protected behind an `Arc` which means
        // there there can be many references to it. We are guaranteed, though,
        // that each reference to the underlying `mmap` has a disjoint `range`
        // listed that it can access. This means that despite having shared
        // access to the mmap itself we have exclusive ownership of the bytes
        // specified in `self.range`. This should allow us to safely hand out
        // mutable access to these bytes if so desired.
        unsafe {
            let slice = std::slice::from_raw_parts_mut(self.mmap.as_mut_ptr(), self.mmap.len());
            &mut slice[self.range.clone()]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MmapVec;

    #[test]
    fn smoke() {
        let mut mmap = MmapVec::with_capacity(10).unwrap();
        assert_eq!(mmap.len(), 10);
        assert_eq!(&mmap[..], &[0; 10]);

        mmap[0] = 1;
        mmap[2] = 3;
        assert!(mmap.get(10).is_none());
        assert_eq!(mmap[0], 1);
        assert_eq!(mmap[2], 3);
    }

    #[test]
    fn split_off() {
        let mut vec = Vec::from([1, 2, 3, 4]);
        let mut mmap = MmapVec::from_slice(&vec).unwrap();
        assert_eq!(&mmap[..], &vec[..]);
        // remove nothing; vec length remains 4
        assert_eq!(&mmap.split_off(4)[..], &vec.split_off(4)[..]);
        assert_eq!(&mmap[..], &vec[..]);
        // remove 1 element; vec length is now 3
        assert_eq!(&mmap.split_off(3)[..], &vec.split_off(3)[..]);
        assert_eq!(&mmap[..], &vec[..]);
        // remove 2 elements; vec length is now 1
        assert_eq!(&mmap.split_off(1)[..], &vec.split_off(1)[..]);
        assert_eq!(&mmap[..], &vec[..]);
        // remove last element; vec length is now 0
        assert_eq!(&mmap.split_off(0)[..], &vec.split_off(0)[..]);
        assert_eq!(&mmap[..], &vec[..]);
        // nothing left to remove, but that's okay
        assert_eq!(&mmap.split_off(0)[..], &vec.split_off(0)[..]);
        assert_eq!(&mmap[..], &vec[..]);
    }
}
