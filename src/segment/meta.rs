// Copyright (c) 2024-present, fjall-rs
// This source code is licensed under both the Apache 2.0 and MIT License
// (found in the LICENSE-* files in the repository)

use super::{trailer::Trailer, Block, DataBlock};
use crate::{coding::Decode, CompressionType, KeyRange, SegmentId, SeqNo};
use byteorder::{LittleEndian, ReadBytesExt};
use std::{fs::File, ops::Deref};

/// Nano-second timestamp.
pub struct Timestamp(u128);

impl Deref for Timestamp {
    type Target = u128;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Timestamp> for u128 {
    fn from(val: Timestamp) -> Self {
        val.0
    }
}

impl From<u128> for Timestamp {
    fn from(value: u128) -> Self {
        Self(value)
    }
}

pub struct ParsedMeta {
    pub id: SegmentId,
    pub created_at: Timestamp,
    pub data_block_count: u64,
    pub index_block_count: u64,
    pub key_range: KeyRange,
    pub seqnos: (SeqNo, SeqNo),
    pub file_size: u64,
    pub item_count: u64,

    pub data_block_compression: CompressionType,
}

impl ParsedMeta {
    #[allow(clippy::expect_used)]
    pub fn from_trailer(file: &File, trailer: &Trailer) -> crate::Result<Self> {
        let ptr = trailer.metadata;
        let block = Block::from_file(file, ptr.offset(), ptr.size(), CompressionType::None)?;
        let block = DataBlock::new(block);

        assert_eq!(
            b"xxh3",
            &*block
                .point_read(b"#hash_type", None)
                .expect("Segment ID should exist")
                .value,
            "invalid hash type",
        );

        assert_eq!(
            b"xxh3",
            &*block
                .point_read(b"#checksum_type", None)
                .expect("Segment ID should exist")
                .value,
            "invalid checksum type",
        );

        let id = {
            let bytes = block
                .point_read(b"#id", None)
                .expect("Segment ID should exist");

            let mut bytes = &bytes.value[..];
            bytes.read_u64::<LittleEndian>()?
        };

        let created_at = {
            let bytes = block
                .point_read(b"#created_at", None)
                .expect("Segment created_at should exist");

            let mut bytes = &bytes.value[..];
            bytes.read_u128::<LittleEndian>()?.into()
        };

        let item_count = {
            let bytes = block
                .point_read(b"#item_count", None)
                .expect("Segment ID should exist");

            let mut bytes = &bytes.value[..];
            bytes.read_u64::<LittleEndian>()?
        };

        let data_block_count = {
            let bytes = block
                .point_read(b"#data_block_count", None)
                .expect("data_block_count should exist");

            let mut bytes = &bytes.value[..];
            bytes.read_u64::<LittleEndian>()?
        };

        let index_block_count = {
            let bytes = block
                .point_read(b"#index_block_count", None)
                .expect("index_block_count should exist");

            let mut bytes = &bytes.value[..];
            bytes.read_u64::<LittleEndian>()?
        };

        let key_range = KeyRange::new((
            block
                .point_read(b"#key#min", None)
                .expect("key min should exist")
                .value,
            block
                .point_read(b"#key#max", None)
                .expect("key max should exist")
                .value,
        ));

        let seqnos = {
            let min = {
                let bytes = block
                    .point_read(b"#seqno#min", None)
                    .expect("seqno min should exist")
                    .value;
                let mut bytes = &bytes[..];
                bytes.read_u64::<LittleEndian>()?
            };

            let max = {
                let bytes = block
                    .point_read(b"#seqno#max", None)
                    .expect("seqno max should exist")
                    .value;
                let mut bytes = &bytes[..];
                bytes.read_u64::<LittleEndian>()?
            };

            (min, max)
        };

        let file_size = {
            let bytes = block.point_read(b"#size", None).expect("size should exist");
            let mut bytes = &bytes.value[..];
            bytes.read_u64::<LittleEndian>()?
        };

        let data_block_compression = {
            let bytes = block
                .point_read(b"#compression#data", None)
                .expect("size should exist");

            let mut bytes = &bytes.value[..];
            CompressionType::decode_from(&mut bytes)?
        };

        Ok(Self {
            id,
            created_at,
            data_block_count,
            index_block_count,
            key_range,
            seqnos,
            file_size,
            item_count,
            data_block_compression,
        })
    }
}
