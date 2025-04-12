// Copyright (c) 2025-present, fjall-rs
// This source code is licensed under both the Apache 2.0 and MIT License
// (found in the LICENSE-* files in the repository)

mod block_handle;

pub use block_handle::{NewBlockHandle, NewKeyedBlockHandle};

use super::{
    block::{binary_index::Reader as BinaryIndexReader, BlockOffset, Encoder, Trailer},
    Block,
};
use byteorder::{LittleEndian, ReadBytesExt};
use std::io::{Cursor, Seek};
use varint_rs::VarintReader;

macro_rules! unwrappy {
    ($x:expr) => {
        // $x.expect("should read")

        unsafe { $x.unwrap_unchecked() }
    };
}

/// Block that contains block handles (file offset + size)
pub struct IndexBlock {
    pub inner: Block,

    // Cached metadata
    restart_interval: u8,

    binary_index_step_size: u8,
    binary_index_offset: u32,
    binary_index_len: u32,
}

struct RestartHead {
    offset: BlockOffset,
    size: u32,
    key_start: usize,
    key_len: usize,
}

impl IndexBlock {
    #[must_use]
    pub fn new(inner: Block) -> Self {
        let trailer = Trailer::new(&inner);
        let mut reader = trailer.as_slice();

        let _item_count = reader.read_u32::<LittleEndian>().expect("should read");

        let restart_interval = unwrappy!(reader.read_u8());

        let binary_index_step_size = unwrappy!(reader.read_u8());

        debug_assert!(
            binary_index_step_size == 2 || binary_index_step_size == 4,
            "invalid binary index step size",
        );

        let binary_index_offset = unwrappy!(reader.read_u32::<LittleEndian>());
        let binary_index_len = unwrappy!(reader.read_u32::<LittleEndian>());

        Self {
            inner,

            restart_interval,

            binary_index_step_size,
            binary_index_offset,
            binary_index_len,
        }
    }

    /// Returns the amount of items in the block
    #[must_use]
    pub fn item_count(&self) -> usize {
        Trailer::new(&self.inner).item_count()
    }

    /// Always returns false: a block is never empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Access the inner raw bytes
    #[must_use]
    fn bytes(&self) -> &[u8] {
        &self.inner.data
    }

    /// Returns the binary index length (number of pointers).
    ///
    /// The number of pointers is equal to the number of restart intervals.
    #[must_use]
    pub fn binary_index_len(&self) -> u32 {
        self.binary_index_len
    }

    /// Returns the binary index offset.
    #[must_use]
    fn binary_index_offset(&self) -> u32 {
        self.binary_index_offset
    }

    /// Returns the binary index step size.
    ///
    /// The binary index can either store u16 or u32 pointers,
    /// depending on the size of the data block.
    ///
    /// Typically blocks are < 64K, so u16 pointers reduce the index
    /// size by half.
    #[must_use]
    fn binary_index_step_size(&self) -> u8 {
        self.binary_index_step_size
    }

    fn get_binary_index_reader(&self) -> BinaryIndexReader {
        BinaryIndexReader::new(
            self.bytes(),
            self.binary_index_offset(),
            self.binary_index_len(),
            self.binary_index_step_size(),
        )
    }

    fn parse_restart_head(cursor: &mut Cursor<&[u8]>, pos: usize) -> RestartHead {
        let offset = unwrappy!(cursor.read_u64_varint());
        let size = unwrappy!(cursor.read_u32_varint());

        let key_len: usize = unwrappy!(cursor.read_u16_varint()).into();
        let key_start = pos + cursor.position() as usize;

        unwrappy!(cursor.seek_relative(key_len as i64));

        RestartHead {
            offset: BlockOffset(offset),
            size,
            key_start,
            key_len,
        }
    }

    fn get_key_at(&self, pos: usize) -> &[u8] {
        let bytes = &self.inner.data;

        // SAFETY: pos is always retrieved from the binary index,
        // which we consider to be trustworthy
        #[warn(unsafe_code)]
        let mut cursor = Cursor::new(unsafe { bytes.get_unchecked(pos..) });

        // TODO: maybe move these behind the key
        let _ = unwrappy!(cursor.read_u64_varint());
        let _ = unwrappy!(cursor.read_u32_varint());

        let key_len: usize = unwrappy!(cursor.read_u16_varint()).into();
        let key_start = cursor.position() as usize;

        let key_start = pos + key_start;
        let key_end = key_start + key_len;

        #[warn(unsafe_code)]
        let key = bytes.get(key_start..key_end).expect("should read");

        key
    }

    /* fn walk(
        &self,
        needle: &[u8],
        pos: usize,
        restart_interval: usize,
    ) -> crate::Result<Option<NewKeyedBlockHandle>> {
        use std::cmp::Ordering::{Equal, Greater, Less};

        let bytes = &self.inner.data;
        let mut cursor = Cursor::new(&bytes[pos..]);

        let mut base_key_pos = 0;
        let mut offset = BlockOffset(0);

        // NOTE: Check the full item
        let base_key = {
            let parsed = unwrappy!(Self::parse_restart_head(&mut cursor));

            let key_start = pos + parsed.key_start;
            let key_end = key_start + parsed.key_len;
            let key = &bytes[key_start..key_end];

            match key.cmp(needle) {
                Equal => {
                    let key = bytes.slice(key_start..key_end);

                    return Ok(Some(NewKeyedBlockHandle {
                        end_key: key,
                        offset: parsed.offset,
                        size: parsed.size,
                    }));
                }
                Greater => {
                    // NOTE: Already passed searched key
                    return Ok(None);
                }
                Less => {
                    // NOTE: Continue
                }
            }

            base_key_pos = key_start;
            offset = BlockOffset(*parsed.offset + u64::from(parsed.size));

            key
        };

        // NOTE: Check the rest items
        for _idx in 1..restart_interval {
            let size = cursor.read_u32_varint()?;

            let shared_prefix_len: usize = unwrappy!(cursor.read_u16_varint()).into();
            let rest_key_len: usize = unwrappy!(cursor.read_u16_varint()).into();

            let key_offset = pos + cursor.position() as usize;

            // NOTE: PERF: Slicing seems to be faster than get_unchecked!!
            let prefix_part = &base_key[0..shared_prefix_len];
            let rest_key = &bytes[key_offset..(key_offset + rest_key_len)];

            unwrappy!(cursor.seek_relative(rest_key_len as i64));

            match compare_prefixed_slice(prefix_part, rest_key, needle) {
                Equal => {
                    let key = if shared_prefix_len == 0 {
                        bytes.slice(key_offset..(key_offset + rest_key_len))
                    } else if rest_key_len == 0 {
                        bytes.slice(base_key_pos..(base_key_pos + shared_prefix_len))
                    } else {
                        // Stitch key
                        UserKey::fused(prefix_part, rest_key)
                    };

                    return Ok(Some(NewKeyedBlockHandle {
                        end_key: key,
                        offset,
                        size,
                    }));
                }
                Greater => {
                    // NOTE: Already passed searched key
                    return Ok(None);
                }
                Less => {
                    // NOTE: Continue
                }
            }

            offset += u64::from(size);
        }

        Ok(None)
    } */

    /// Search for the lowest block that may possibly contain the needle.
    fn search_lowest(&self, binary_index: &BinaryIndexReader, needle: &[u8]) -> Option<usize> {
        let mut left: usize = 0;
        let mut right = binary_index.len();

        if right == 0 {
            return None;
        }

        while left < right {
            let mid = (left + right) / 2;

            let offset = binary_index.get(mid);

            if self.get_key_at(offset) < needle {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        Some(if left < binary_index.len() {
            binary_index.get(left)
        } else {
            binary_index.get(binary_index.len() - 1)
        })
    }

    /// Search for the last block that may possibly contain the needle.
    fn search_highest(&self, binary_index: &BinaryIndexReader, needle: &[u8]) -> Option<usize> {
        let mut left: usize = 0;
        let mut right = binary_index.len();

        if right == 0 {
            return None;
        }

        while left < right {
            let mid = (left + right) / 2;

            let offset = binary_index.get(mid);

            if self.get_key_at(offset) <= needle {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        if left == 0 {
            Some(binary_index.get(0))
        } else if left == binary_index.len() {
            Some(binary_index.get(binary_index.len() - 1))
        } else {
            Some(binary_index.get(left))
        }
    }

    #[must_use]
    pub fn get_lowest_possible_block(&self, needle: &[u8]) -> Option<NewKeyedBlockHandle> {
        let binary_index = self.get_binary_index_reader();

        /*
         // NOTE: Currently, the hash index is never initialized for index blocks
         /*  // NOTE: Try hash index if it exists
         if let Some(bucket_value) = self
             .get_hash_index_reader()
             .and_then(|reader| reader.get(key))
         {
             let restart_entry_pos = binary_index.get(usize::from(bucket_value));
             return self.walk(key, seqno, restart_entry_pos, self.restart_interval.into());
         } */
        ) */

        let offset = self.search_lowest(&binary_index, needle)?;

        // SAFETY: pos is always retrieved from the binary index,
        // which we consider to be trustworthy
        #[warn(unsafe_code)]
        let mut cursor = Cursor::new(unsafe { self.inner.data.get_unchecked(offset..) });

        let item = Self::parse_restart_head(&mut cursor, offset);

        let end_key = self
            .inner
            .data
            .slice(item.key_start..(item.key_start + item.key_len));

        if needle > end_key {
            return None;
        }

        Some(NewKeyedBlockHandle::new(end_key, item.offset, item.size))
    }

    #[must_use]
    pub fn get_highest_possible_block(&self, needle: &[u8]) -> Option<NewKeyedBlockHandle> {
        let binary_index = self.get_binary_index_reader();

        /*
         // NOTE: Currently, the hash index is never initialized for index blocks
         /*  // NOTE: Try hash index if it exists
         if let Some(bucket_value) = self
             .get_hash_index_reader()
             .and_then(|reader| reader.get(key))
         {
             let restart_entry_pos = binary_index.get(usize::from(bucket_value));
             return self.walk(key, seqno, restart_entry_pos, self.restart_interval.into());
         } */
        ) */

        let offset = self.search_highest(&binary_index, needle)?;

        // SAFETY: pos is always retrieved from the binary index,
        // which we consider to be trustworthy
        #[warn(unsafe_code)]
        let mut cursor = Cursor::new(unsafe { self.inner.data.get_unchecked(offset..) });

        let item = Self::parse_restart_head(&mut cursor, offset);

        let end_key = self
            .inner
            .data
            .slice(item.key_start..(item.key_start + item.key_len));

        if needle > end_key {
            return None;
        }

        Some(NewKeyedBlockHandle::new(end_key, item.offset, item.size))
    }

    pub fn encode_items(items: &[NewKeyedBlockHandle]) -> crate::Result<Vec<u8>> {
        let first_key = items.first().expect("chunk should not be empty").end_key();

        let mut serializer = Encoder::<'_, BlockOffset, NewKeyedBlockHandle>::new(
            items.len(),
            1,   // TODO: hard-coded for now
            0.0, // TODO: hard-coded for now
            first_key,
        );

        for item in items {
            serializer.write(item)?;
        }

        serializer.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::super_segment::block::{Checksum, Header};
    use test_log::test;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn v3_index_block_simple() -> crate::Result<()> {
        let items = [
            NewKeyedBlockHandle::new(b"b".into(), BlockOffset(0), 6_000),
            NewKeyedBlockHandle::new(b"bcdef".into(), BlockOffset(6_000), 7_000),
            NewKeyedBlockHandle::new(b"def".into(), BlockOffset(13_000), 5_000),
        ];

        let bytes = IndexBlock::encode_items(&items)?;
        eprintln!("{bytes:?}");
        eprintln!("{}", String::from_utf8_lossy(&bytes));
        /* eprintln!("encoded into {} bytes", bytes.len()); */

        let data_block = IndexBlock::new(Block {
            data: bytes.into(),
            header: Header {
                checksum: Checksum::from_raw(0),
                data_length: 0,
                uncompressed_length: 0,
                previous_block_offset: BlockOffset(0),
            },
        });

        assert_eq!(data_block.item_count(), items.len());

        assert_eq!(
            Some(items.first().unwrap().clone()),
            data_block.get_lowest_possible_block(b"a")
        );
        assert_eq!(
            Some(items.first().unwrap().clone()),
            data_block.get_lowest_possible_block(b"b")
        );
        assert_eq!(
            Some(items.get(1).unwrap().clone()),
            data_block.get_lowest_possible_block(b"ba")
        );
        assert_eq!(
            Some(items.get(2).unwrap().clone()),
            data_block.get_lowest_possible_block(b"d")
        );

        // assert_eq!(None, data_block.get_lowest_possible_block(b"zzz"));

        Ok(())
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn v3_index_block_span() -> crate::Result<()> {
        let items = [
            NewKeyedBlockHandle::new(b"a".into(), BlockOffset(0), 6_000),
            NewKeyedBlockHandle::new(b"a".into(), BlockOffset(6_000), 7_000),
            NewKeyedBlockHandle::new(b"b".into(), BlockOffset(13_000), 5_000),
        ];

        let bytes = IndexBlock::encode_items(&items)?;
        // eprintln!("{bytes:?}");
        // eprintln!("{}", String::from_utf8_lossy(&bytes));
        /* eprintln!("encoded into {} bytes", bytes.len()); */

        let data_block = IndexBlock::new(Block {
            data: bytes.into(),
            header: Header {
                checksum: Checksum::from_raw(0),
                data_length: 0,
                uncompressed_length: 0,
                previous_block_offset: BlockOffset(0),
            },
        });

        assert_eq!(data_block.item_count(), items.len());

        assert_eq!(
            Some(items.first().unwrap().clone()),
            data_block.get_lowest_possible_block(b"a")
        );
        assert_eq!(
            Some(items.last().unwrap().clone()),
            data_block.get_lowest_possible_block(b"abc")
        );
        assert_eq!(
            Some(items.last().unwrap().clone()),
            data_block.get_lowest_possible_block(b"b")
        );

        Ok(())
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn v3_index_block_span_highest() -> crate::Result<()> {
        let items = [
            NewKeyedBlockHandle::new(b"b".into(), BlockOffset(0), 6_000),
            NewKeyedBlockHandle::new(b"c".into(), BlockOffset(0), 6_000),
            NewKeyedBlockHandle::new(b"c".into(), BlockOffset(6_000), 7_000),
            NewKeyedBlockHandle::new(b"d".into(), BlockOffset(13_000), 5_000),
        ];

        let bytes = IndexBlock::encode_items(&items)?;
        // eprintln!("{bytes:?}");
        // eprintln!("{}", String::from_utf8_lossy(&bytes));
        /* eprintln!("encoded into {} bytes", bytes.len()); */

        let data_block = IndexBlock::new(Block {
            data: bytes.into(),
            header: Header {
                checksum: Checksum::from_raw(0),
                data_length: 0,
                uncompressed_length: 0,
                previous_block_offset: BlockOffset(0),
            },
        });

        assert_eq!(data_block.item_count(), items.len());

        assert_eq!(
            Some(items.first().unwrap().clone()),
            data_block.get_highest_possible_block(b"a")
        );
        assert_eq!(
            Some(items.get(1).unwrap().clone()),
            data_block.get_highest_possible_block(b"abc")
        );
        assert_eq!(
            Some(items.last().unwrap().clone()),
            data_block.get_highest_possible_block(b"c")
        );
        assert_eq!(
            Some(items.last().unwrap().clone()),
            data_block.get_highest_possible_block(b"cef")
        );
        assert_eq!(
            Some(items.last().unwrap().clone()),
            data_block.get_highest_possible_block(b"d")
        );
        assert_eq!(None, data_block.get_highest_possible_block(b"zzz"));

        Ok(())
    }

    #[test]
    fn v3_index_block_one() -> crate::Result<()> {
        let item = NewKeyedBlockHandle::new(b"c".into(), BlockOffset(0), 6_000);

        let bytes = IndexBlock::encode_items(&[item.clone()])?;
        // eprintln!("{bytes:?}");
        // eprintln!("{}", String::from_utf8_lossy(&bytes));
        /* eprintln!("encoded into {} bytes", bytes.len()); */

        let data_block = IndexBlock::new(Block {
            data: bytes.into(),
            header: Header {
                checksum: Checksum::from_raw(0),
                data_length: 0,
                uncompressed_length: 0,
                previous_block_offset: BlockOffset(0),
            },
        });

        assert_eq!(data_block.item_count(), 1);

        assert_eq!(
            Some(item.clone()),
            data_block.get_lowest_possible_block(b"a")
        );
        assert_eq!(
            Some(item.clone()),
            data_block.get_lowest_possible_block(b"asdasd")
        );
        assert_eq!(
            Some(item.clone()),
            data_block.get_lowest_possible_block(b"b")
        );
        assert_eq!(Some(item), data_block.get_lowest_possible_block(b"c"));
        assert_eq!(None, data_block.get_lowest_possible_block(b"d"));
        assert_eq!(None, data_block.get_lowest_possible_block(b"z"));

        Ok(())
    }

    #[test]
    fn v3_index_block_one_highest() -> crate::Result<()> {
        let item = NewKeyedBlockHandle::new(b"c".into(), BlockOffset(0), 6_000);

        let bytes = IndexBlock::encode_items(&[item.clone()])?;
        // eprintln!("{bytes:?}");
        // eprintln!("{}", String::from_utf8_lossy(&bytes));
        /* eprintln!("encoded into {} bytes", bytes.len()); */

        let data_block = IndexBlock::new(Block {
            data: bytes.into(),
            header: Header {
                checksum: Checksum::from_raw(0),
                data_length: 0,
                uncompressed_length: 0,
                previous_block_offset: BlockOffset(0),
            },
        });

        assert_eq!(data_block.item_count(), 1);

        assert_eq!(
            Some(item.clone()),
            data_block.get_lowest_possible_block(b"a")
        );
        assert_eq!(
            Some(item.clone()),
            data_block.get_lowest_possible_block(b"asdasd")
        );
        assert_eq!(
            Some(item.clone()),
            data_block.get_lowest_possible_block(b"b")
        );
        assert_eq!(Some(item), data_block.get_lowest_possible_block(b"c"));
        assert_eq!(None, data_block.get_lowest_possible_block(b"d"));
        assert_eq!(None, data_block.get_lowest_possible_block(b"z"));

        Ok(())
    }
}
