// Copyright (c) 2024-present, fjall-rs
// This source code is licensed under both the Apache 2.0 and MIT License
// (found in the LICENSE-* files in the repository)

use super::{Choice, CompactionStrategy, Input as CompactionInput};
use crate::{level_manifest::LevelManifest, segment::Segment, Config, HashSet};

fn desired_level_size_in_bytes(level_idx: u8, ratio: u8, base_size: u32) -> usize {
    (ratio as usize).pow(u32::from(level_idx + 1)) * (base_size as usize)
}

/// Size-tiered compaction strategy (STCS)
///
/// If a level reaches a threshold, it is merged into a larger segment to the next level.
///
/// STCS suffers from high read and temporary doubled space amplification, but has good write amplification.
#[derive(Clone)]
pub struct Strategy {
    /// Base size
    pub base_size: u32,

    /// Size ratio between levels of the LSM tree (a.k.a fanout, growth rate).
    ///
    /// This is the exponential growth of the from one
    /// level to the next
    ///
    /// A level target size is: base_size * level_ratio.pow(#level + 1)
    #[allow(clippy::doc_markdown)]
    pub level_ratio: u8,
}

impl Strategy {
    /// Creates a new STCS strategy with custom base size
    #[must_use]
    pub fn new(base_size: u32, level_ratio: u8) -> Self {
        Self {
            base_size,
            level_ratio,
        }
    }
}

impl Default for Strategy {
    fn default() -> Self {
        Self {
            base_size: 64 * 1_024 * 1_024,
            level_ratio: 4,
        }
    }
}

impl CompactionStrategy for Strategy {
    fn get_name(&self) -> &'static str {
        "TieredStrategy"
    }

    fn choose(&self, levels: &LevelManifest, config: &Config) -> Choice {
        let resolved_view = levels.resolved_view();

        for (curr_level_index, level) in resolved_view
            .iter()
            .enumerate()
            .take(resolved_view.len() - 1)
            .rev()
        {
            // NOTE: Level count is 255 max
            #[allow(clippy::cast_possible_truncation)]
            let curr_level_index = curr_level_index as u8;

            let next_level_index = curr_level_index + 1;

            if level.is_empty() {
                continue;
            }

            let level_size: u64 = level
                .segments
                .iter()
                // NOTE: Take bytes that are already being compacted into account,
                // otherwise we may be overcompensating
                .filter(|x| !levels.hidden_set().is_hidden(x.id()))
                .map(|x| x.metadata.file_size)
                .sum();

            let desired_bytes =
                desired_level_size_in_bytes(curr_level_index, self.level_ratio, self.base_size)
                    as u64;

            if level_size >= desired_bytes {
                // NOTE: Take desired_bytes because we are in tiered mode
                // We want to take N segments, not just the overshoot (like in leveled)
                let mut overshoot = desired_bytes;

                let mut segments_to_compact = vec![];

                for segment in level.iter().rev().take(self.level_ratio.into()).cloned() {
                    if overshoot == 0 {
                        break;
                    }

                    overshoot = overshoot.saturating_sub(segment.metadata.file_size);
                    segments_to_compact.push(segment);
                }

                let mut segment_ids: HashSet<_> =
                    segments_to_compact.iter().map(Segment::id).collect();

                // NOTE: If dest level is the last level, just overwrite it
                //
                // If we didn't overwrite Lmax, it would end up amassing more and more
                // segments
                // Also, because it's the last level, the frequency of overwiting it is
                // amortized because of the LSM-tree's level structure
                if next_level_index == 6 {
                    // Wait for L6 to be non-busy
                    if levels.busy_levels().contains(&next_level_index) {
                        continue;
                    }

                    segment_ids.extend(
                        levels
                            .levels
                            .last()
                            .expect("last level should always exist")
                            .list_ids(),
                    );
                }

                return Choice::Merge(CompactionInput {
                    segment_ids,
                    dest_level: next_level_index,
                    target_size: u64::MAX,
                });
            }
        }

        // TODO: after major compaction, SizeTiered may behave weirdly
        // if major compaction is not outputting into Lmax

        // TODO: if level.size >= base_size and there are enough
        // segments with size < base_size, compact them together
        // no matter the amount of segments in L0 -> should reduce
        // write stall chance
        //
        // TODO: however: force compaction if L0 becomes way too large

        // NOTE: Reduce L0 segments if needed
        // this is probably an edge case if the `base_size` does not line up with
        // the `max_memtable_size` AT ALL
        super::maintenance::Strategy.choose(levels, config)
    }
}
/*
#[cfg(test)]
mod tests {
    use super::Strategy;
    use crate::{
        cache::Cache,
        compaction::{Choice, CompactionStrategy, Input as CompactionInput},
        config::Config,
        descriptor_table::FileDescriptorTable,
        file::LEVELS_MANIFEST_FILE,
        level_manifest::LevelManifest,
        segment::{
            block::offset::BlockOffset,
            block_index::{two_level_index::TwoLevelBlockIndex, BlockIndexImpl},
            file_offsets::FileOffsets,
            meta::{Metadata, SegmentId},
            SegmentInner,
        },
        super_segment::Segment,
        HashSet, KeyRange, SeqNo,
    };
    use std::sync::{atomic::AtomicBool, Arc};
    use test_log::test;

    #[allow(clippy::expect_used)]
    fn fixture_segment(id: SegmentId, size_mib: u64, max_seqno: SeqNo) -> Segment {
        todo!()

        /* let cache = Arc::new(Cache::with_capacity_bytes(10 * 1_024 * 1_024));

        let block_index = TwoLevelBlockIndex::new((0, id).into(), cache.clone());
        let block_index = Arc::new(BlockIndexImpl::TwoLevel(block_index));

        SegmentInner {
            tree_id: 0,
            descriptor_table: Arc::new(FileDescriptorTable::new(512, 1)),
            block_index,

            offsets: FileOffsets {
                bloom_ptr: BlockOffset(0),
                range_filter_ptr: BlockOffset(0),
                index_block_ptr: BlockOffset(0),
                metadata_ptr: BlockOffset(0),
                range_tombstones_ptr: BlockOffset(0),
                tli_ptr: BlockOffset(0),
                pfx_ptr: BlockOffset(0),
            },

            metadata: Metadata {
                data_block_count: 0,
                index_block_count: 0,
                data_block_size: 4_096,
                index_block_size: 4_096,
                created_at: 0,
                id,
                file_size: size_mib * 1_024 * 1_024,
                compression: crate::segment::meta::CompressionType::None,
                table_type: crate::segment::meta::TableType::Block,
                item_count: 0,
                key_count: 0,
                key_range: KeyRange::new((vec![].into(), vec![].into())),
                tombstone_count: 0,
                range_tombstone_count: 0,
                uncompressed_size: size_mib * 1_024 * 1_024,
                seqnos: (0, max_seqno),
            },
            cache,

            bloom_filter: Some(BloomFilter::with_fp_rate(1, 0.1)),

            path: "a".into(),
            is_deleted: AtomicBool::default(),
        }
        .into() */
    }

    #[test]
    fn tiered_empty_levels() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;

        let compactor = Strategy {
            base_size: 8 * 1_024 * 1_024,
            level_ratio: 8,
        };

        let levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;

        assert_eq!(
            compactor.choose(&levels, &Config::default()),
            Choice::DoNothing
        );

        Ok(())
    }

    #[test]
    fn tiered_default_l0() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;

        let compactor = Strategy {
            base_size: 8 * 1_024 * 1_024,
            level_ratio: 4,
        };
        let config = Config::default();

        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;

        levels.add(fixture_segment(1, 8, 5));
        assert_eq!(compactor.choose(&levels, &config), Choice::DoNothing);

        levels.add(fixture_segment(2, 8, 6));
        assert_eq!(compactor.choose(&levels, &config), Choice::DoNothing);

        levels.add(fixture_segment(3, 8, 7));
        assert_eq!(compactor.choose(&levels, &config), Choice::DoNothing);

        levels.add(fixture_segment(4, 8, 8));

        assert_eq!(
            compactor.choose(&levels, &config),
            Choice::Merge(CompactionInput {
                dest_level: 1,
                segment_ids: set![1, 2, 3, 4],
                target_size: u64::MAX,
            })
        );

        Ok(())
    }

    #[test]
    fn tiered_ordering() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;

        let compactor = Strategy {
            base_size: 8 * 1_024 * 1_024,
            level_ratio: 2,
        };
        let config = Config::default();

        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;

        levels.add(fixture_segment(1, 8, 0));
        levels.add(fixture_segment(2, 8, 1));
        levels.add(fixture_segment(3, 8, 2));
        levels.add(fixture_segment(4, 8, 3));

        assert_eq!(
            compactor.choose(&levels, &config),
            Choice::Merge(CompactionInput {
                dest_level: 1,
                segment_ids: set![1, 2],
                target_size: u64::MAX,
            })
        );

        Ok(())
    }

    #[test]
    fn tiered_more_than_min() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;

        let compactor = Strategy {
            base_size: 8 * 1_024 * 1_024,
            level_ratio: 4,
        };
        let config = Config::default();

        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;
        levels.add(fixture_segment(1, 8, 5));
        levels.add(fixture_segment(2, 8, 6));
        levels.add(fixture_segment(3, 8, 7));
        levels.add(fixture_segment(4, 8, 8));

        levels.insert_into_level(1, fixture_segment(5, 8 * 4, 9));
        levels.insert_into_level(1, fixture_segment(6, 8 * 4, 10));
        levels.insert_into_level(1, fixture_segment(7, 8 * 4, 11));
        levels.insert_into_level(1, fixture_segment(8, 8 * 4, 12));

        assert_eq!(
            compactor.choose(&levels, &config),
            Choice::Merge(CompactionInput {
                dest_level: 2,
                segment_ids: set![5, 6, 7, 8],
                target_size: u64::MAX,
            })
        );

        Ok(())
    }

    #[test]
    fn tiered_many_segments() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;

        let compactor = Strategy {
            base_size: 8 * 1_024 * 1_024,
            level_ratio: 2,
        };
        let config = Config::default();

        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;
        levels.add(fixture_segment(1, 8, 5));
        levels.add(fixture_segment(2, 8, 6));
        levels.add(fixture_segment(3, 8, 7));
        levels.add(fixture_segment(4, 8, 8));

        assert_eq!(
            compactor.choose(&levels, &config),
            Choice::Merge(CompactionInput {
                dest_level: 1,
                segment_ids: set![1, 2],
                target_size: u64::MAX,
            })
        );

        Ok(())
    }

    #[test]
    fn tiered_deeper_level() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;

        let compactor = Strategy {
            base_size: 8 * 1_024 * 1_024,
            level_ratio: 2,
        };
        let config = Config::default();

        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;
        levels.add(fixture_segment(1, 8, 5));

        levels.insert_into_level(1, fixture_segment(2, 8 * 2, 6));
        levels.insert_into_level(1, fixture_segment(3, 8 * 2, 7));

        assert_eq!(
            compactor.choose(&levels, &config),
            Choice::Merge(CompactionInput {
                dest_level: 2,
                segment_ids: set![2, 3],
                target_size: u64::MAX,
            })
        );

        let tempdir = tempfile::tempdir()?;
        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;

        levels.insert_into_level(2, fixture_segment(2, 8 * 4, 5));
        levels.insert_into_level(2, fixture_segment(3, 8 * 4, 6));

        assert_eq!(
            compactor.choose(&levels, &config),
            Choice::Merge(CompactionInput {
                dest_level: 3,
                segment_ids: set![2, 3],
                target_size: u64::MAX,
            })
        );

        Ok(())
    }

    #[test]
    fn tiered_last_level() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;

        let compactor = Strategy {
            base_size: 8 * 1_024 * 1_024,
            level_ratio: 2,
        };
        let config = Config::default();

        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;
        levels.insert_into_level(3, fixture_segment(2, 8, 5));
        levels.insert_into_level(3, fixture_segment(3, 8, 5));

        assert_eq!(compactor.choose(&levels, &config), Choice::DoNothing);

        Ok(())
    }
}
 */
