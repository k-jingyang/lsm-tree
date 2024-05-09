use crate::{
    descriptor_table::FileDescriptorTable,
    file::BLOCKS_FILE,
    memtable::MemTable,
    segment::{
        block_index::BlockIndex,
        meta::{Metadata, SegmentId},
        writer::Writer,
        Segment,
    },
    tree_inner::TreeId,
    BlockCache,
};
use std::{path::PathBuf, sync::Arc};

#[cfg(feature = "bloom")]
use crate::bloom::BloomFilter;

#[cfg(feature = "bloom")]
use crate::file::BLOOM_FILTER_FILE;

/// Flush options
#[doc(hidden)]
pub struct Options {
    /// [`MemTable`] to flush
    pub memtable: Arc<MemTable>,

    /// Tree ID
    pub tree_id: TreeId,

    /// Unique segment ID
    pub segment_id: SegmentId,

    /// Base folder of segments
    ///
    /// The segment will be stored in {folder}/{segment_id}
    #[allow(clippy::doc_markdown)]
    pub folder: PathBuf,

    /// Block size in bytes
    pub block_size: u32,

    // Block cache
    pub block_cache: Arc<BlockCache>,

    // Descriptor table
    pub descriptor_table: Arc<FileDescriptorTable>,
}

/// Flushes a memtable, creating a segment in the given folder
#[allow(clippy::module_name_repetitions)]
#[doc(hidden)]
pub fn flush_to_segment(opts: Options) -> crate::Result<Segment> {
    let segment_folder = opts.folder.join(opts.segment_id.to_string());
    log::debug!("Flushing segment to {segment_folder:?}");

    let mut segment_writer = Writer::new(crate::segment::writer::Options {
        folder: segment_folder.clone(),
        evict_tombstones: false,
        block_size: opts.block_size,

        #[cfg(feature = "bloom")]
        bloom_fp_rate: 0.0001,
    })?;

    for entry in &opts.memtable.items {
        let key = entry.key();
        let value = entry.value();
        segment_writer.write(crate::Value::from(((key.clone()), value.clone())))?;
    }

    segment_writer.finish()?;

    let metadata = Metadata::from_writer(opts.segment_id, segment_writer)?;
    metadata.write_to_file(&segment_folder)?;

    log::debug!("Finalized segment write at {segment_folder:?}");

    // TODO: if L0, L1, preload block index (non-partitioned)
    let block_index = Arc::new(BlockIndex::from_file(
        (opts.tree_id, opts.segment_id).into(),
        opts.descriptor_table.clone(),
        &segment_folder,
        opts.block_cache.clone(),
    )?);

    let created_segment = Segment {
        tree_id: opts.tree_id,

        descriptor_table: opts.descriptor_table.clone(),
        metadata,
        block_index,
        block_cache: opts.block_cache,

        #[cfg(feature = "bloom")]
        bloom_filter: BloomFilter::from_file(segment_folder.join(BLOOM_FILTER_FILE))?,
    };

    opts.descriptor_table.insert(
        segment_folder.join(BLOCKS_FILE),
        (opts.tree_id, created_segment.metadata.id).into(),
    );

    log::debug!("Flushed segment to {segment_folder:?}");

    Ok(created_segment)
}
