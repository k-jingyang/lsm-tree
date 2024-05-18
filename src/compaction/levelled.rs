use super::{Choice, CompactionStrategy, Input as CompactionInput};
use crate::{config::Config, key_range::KeyRange, levels::LevelManifest, segment::Segment};
use std::{ops::Deref, sync::Arc};

/// Levelled compaction strategy (LCS)
///
/// If a level reaches some threshold size, parts of it are merged into overlapping segments in the next level.
///
/// Each level Ln for n >= 1 can have up to ratio^n segments.
///
/// LCS suffers from high write amplification, but decent read & space amplification.
///
/// More info here: <https://opensource.docs.scylladb.com/stable/cql/compaction.html#leveled-compaction-strategy-lcs>
pub struct Strategy {
    /// When the number of segments in L0 reaches this threshold,
    /// they are merged into L1
    ///
    /// Default = 4
    ///
    /// Same as `level0_file_num_compaction_trigger` in `RocksDB`
    pub l0_threshold: u8,

    /// Target segment size (compressed)
    ///
    /// Default = 64 MiB
    ///
    /// Same as `target_file_size_base` in `RocksDB`
    pub target_size: u32,
}

impl Default for Strategy {
    fn default() -> Self {
        Self {
            l0_threshold: 4,
            target_size: 64 * 1_024 * 1_024,
        }
    }
}

fn aggregate_key_range(segments: &[Arc<Segment>]) -> KeyRange {
    let (mut min, mut max) = segments
        .first()
        .expect("segment should always exist")
        .metadata
        .key_range
        .deref()
        .clone();

    for other in segments.iter().skip(1) {
        if other.metadata.key_range.0 < min {
            min = other.metadata.key_range.0.clone();
        }
        if other.metadata.key_range.1 > max {
            max = other.metadata.key_range.1.clone();
        }
    }

    KeyRange::new((min, max))
}

fn desired_level_size_in_bytes(level_idx: u8, ratio: u8, target_size: u32) -> usize {
    (ratio as usize).pow(u32::from(level_idx)) * (target_size as usize)
}

impl CompactionStrategy for Strategy {
    fn choose(&self, levels: &LevelManifest, config: &Config) -> Choice {
        let resolved_view = levels.resolved_view();

        // If there are any levels that already have a compactor working on it
        // we can't touch those, because that could cause a race condition
        // violating the levelled compaction invariance of having a single sorted
        // run per level
        //
        // TODO: However, this can probably improved by checking two compaction
        // workers just don't cross key ranges
        // If so, we should sort the level(s), because if multiple compaction workers
        // wrote to the same level at the same time, we couldn't guarantee that the levels
        // are sorted in ascending keyspace order (current they are because we write the
        // segments from left to right, so lower key bound + creation date match up)
        let busy_levels = levels.busy_levels();

        for (curr_level_index, level) in resolved_view
            .iter()
            .enumerate()
            .map(|(idx, lvl)| (idx as u8, lvl))
            .skip(1)
            .take(resolved_view.len() - 2)
            .rev()
        {
            let next_level_index = curr_level_index + 1;

            if level.is_empty() {
                continue;
            }

            if busy_levels.contains(&curr_level_index) || busy_levels.contains(&next_level_index) {
                continue;
            }

            let curr_level_bytes = level.size();

            let desired_bytes =
                desired_level_size_in_bytes(curr_level_index, config.level_ratio, self.target_size);

            let mut overshoot = curr_level_bytes.saturating_sub(desired_bytes as u64) as usize;

            if overshoot > 0 {
                let mut segments_to_compact = vec![];

                let mut level = level.clone();
                level.sort_by_key_range();

                for segment in level.iter().take(config.level_ratio.into()).cloned() {
                    if overshoot == 0 {
                        break;
                    }

                    overshoot = overshoot.saturating_sub(segment.metadata.file_size as usize);
                    segments_to_compact.push(segment);
                }

                let Some(next_level) = &resolved_view.get(next_level_index as usize) else {
                    break;
                };

                let key_range = aggregate_key_range(&segments_to_compact);
                let overlapping_segment_ids = next_level.get_overlapping_segments(&key_range);

                let mut segment_ids: Vec<_> = segments_to_compact
                    .iter()
                    .map(|x| &x.metadata.id)
                    .copied()
                    .collect();

                segment_ids.extend(&overlapping_segment_ids);

                let choice = CompactionInput {
                    segment_ids,
                    dest_level: next_level_index,
                    target_size: u64::from(self.target_size),
                };

                if overlapping_segment_ids.is_empty() && level.is_disjoint {
                    return Choice::Move(choice);
                }
                return Choice::Merge(choice);
            }
        }

        {
            let Some(first_level) = resolved_view.first() else {
                return Choice::DoNothing;
            };

            if first_level.len() >= self.l0_threshold.into()
                && !busy_levels.contains(&0)
                && !busy_levels.contains(&1)
            {
                let mut first_level_segments = first_level.deref().clone();
                first_level_segments
                    .sort_by(|a, b| a.metadata.key_range.0.cmp(&b.metadata.key_range.0));

                let Some(next_level) = &resolved_view.get(1) else {
                    return Choice::DoNothing;
                };

                let key_range = aggregate_key_range(&first_level_segments);
                let overlapping_segment_ids = next_level.get_overlapping_segments(&key_range);

                let mut segment_ids = first_level_segments
                    .iter()
                    .map(|x| &x.metadata.id)
                    .copied()
                    .collect::<Vec<_>>();

                segment_ids.extend(overlapping_segment_ids);

                return Choice::Merge(CompactionInput {
                    segment_ids,
                    dest_level: 1,
                    target_size: u64::from(self.target_size),
                });
            }
        }

        Choice::DoNothing
    }
}

#[cfg(test)]
mod tests {
    use super::{Choice, Strategy};
    use crate::{
        block_cache::BlockCache,
        compaction::{CompactionStrategy, Input as CompactionInput},
        descriptor_table::FileDescriptorTable,
        file::LEVELS_MANIFEST_FILE,
        key_range::KeyRange,
        levels::LevelManifest,
        segment::{
            block_index::BlockIndex,
            meta::{Metadata, SegmentId},
            Segment,
        },
        time::unix_timestamp,
        Config,
    };
    use std::sync::Arc;
    use test_log::test;

    #[cfg(feature = "bloom")]
    use crate::bloom::BloomFilter;

    fn string_key_range(a: &str, b: &str) -> KeyRange {
        KeyRange::new((a.as_bytes().into(), b.as_bytes().into()))
    }

    #[allow(clippy::expect_used)]
    fn fixture_segment(id: SegmentId, key_range: KeyRange, size: u64) -> Arc<Segment> {
        let block_cache = Arc::new(BlockCache::with_capacity_bytes(10 * 1_024 * 1_024));

        Arc::new(Segment {
            tree_id: 0,
            descriptor_table: Arc::new(FileDescriptorTable::new(512, 1)),
            block_index: Arc::new(BlockIndex::new((0, id).into(), block_cache.clone())),
            metadata: Metadata {
                block_count: 0,
                block_size: 0,
                created_at: unix_timestamp().as_nanos(),
                id,
                file_size: size,
                compression: crate::segment::meta::CompressionType::Lz4,
                table_type: crate::segment::meta::TableType::Block,
                item_count: 0,
                key_count: 0,
                key_range,
                tombstone_count: 0,
                range_tombstone_count: 0,
                uncompressed_size: 0,
                seqnos: (0, 0),
            },
            block_cache,

            #[cfg(feature = "bloom")]
            bloom_filter: BloomFilter::with_fp_rate(1, 0.1),
        })
    }

    #[test]
    fn levelled_empty_levels() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let compactor = Strategy {
            target_size: 128 * 1_024 * 1_024,
            ..Default::default()
        };

        let levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;

        assert_eq!(
            compactor.choose(&levels, &Config::default()),
            Choice::DoNothing
        );

        Ok(())
    }

    #[test]
    fn levelled_default_l0() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let compactor = Strategy {
            target_size: 128 * 1_024 * 1_024,
            ..Default::default()
        };

        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;

        levels.add(fixture_segment(
            1,
            string_key_range("a", "z"),
            128 * 1_024 * 1_024,
        ));
        assert_eq!(
            compactor.choose(&levels, &Config::default()),
            Choice::DoNothing
        );

        levels.add(fixture_segment(
            2,
            string_key_range("a", "z"),
            128 * 1_024 * 1_024,
        ));
        assert_eq!(
            compactor.choose(&levels, &Config::default()),
            Choice::DoNothing
        );

        levels.add(fixture_segment(
            3,
            string_key_range("a", "z"),
            128 * 1_024 * 1_024,
        ));
        assert_eq!(
            compactor.choose(&levels, &Config::default()),
            Choice::DoNothing
        );

        levels.add(fixture_segment(
            4,
            string_key_range("a", "z"),
            128 * 1_024 * 1_024,
        ));

        assert_eq!(
            compactor.choose(&levels, &Config::default()),
            Choice::Merge(CompactionInput {
                dest_level: 1,
                segment_ids: vec![1, 2, 3, 4],
                target_size: 128 * 1_024 * 1_024
            })
        );

        levels.hide_segments(&[4]);
        assert_eq!(
            compactor.choose(&levels, &Config::default()),
            Choice::DoNothing
        );

        Ok(())
    }

    #[test]
    fn levelled_more_than_min_no_overlap() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let compactor = Strategy {
            target_size: 128 * 1_024 * 1_024,
            ..Default::default()
        };

        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;
        levels.add(fixture_segment(
            1,
            string_key_range("h", "t"),
            128 * 1_024 * 1_024,
        ));
        levels.add(fixture_segment(
            2,
            string_key_range("h", "t"),
            128 * 1_024 * 1_024,
        ));
        levels.add(fixture_segment(
            3,
            string_key_range("h", "t"),
            128 * 1_024 * 1_024,
        ));
        levels.add(fixture_segment(
            4,
            string_key_range("h", "t"),
            128 * 1_024 * 1_024,
        ));

        levels.insert_into_level(
            1,
            fixture_segment(5, string_key_range("a", "g"), 128 * 1_024 * 1_024),
        );
        levels.insert_into_level(
            1,
            fixture_segment(6, string_key_range("a", "g"), 128 * 1_024 * 1_024),
        );
        levels.insert_into_level(
            1,
            fixture_segment(7, string_key_range("y", "z"), 128 * 1_024 * 1_024),
        );
        levels.insert_into_level(
            1,
            fixture_segment(8, string_key_range("y", "z"), 128 * 1_024 * 1_024),
        );

        assert_eq!(
            compactor.choose(&levels, &Config::default()),
            Choice::Merge(CompactionInput {
                dest_level: 1,
                segment_ids: vec![1, 2, 3, 4],
                target_size: 128 * 1_024 * 1_024
            })
        );

        Ok(())
    }

    #[test]
    fn levelled_more_than_min_with_overlap() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let compactor = Strategy {
            target_size: 128 * 1_024 * 1_024,
            ..Default::default()
        };

        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;
        levels.add(fixture_segment(
            1,
            string_key_range("a", "g"),
            128 * 1_024 * 1_024,
        ));
        levels.add(fixture_segment(
            2,
            string_key_range("h", "t"),
            128 * 1_024 * 1_024,
        ));
        levels.add(fixture_segment(
            3,
            string_key_range("i", "t"),
            128 * 1_024 * 1_024,
        ));
        levels.add(fixture_segment(
            4,
            string_key_range("j", "t"),
            128 * 1_024 * 1_024,
        ));

        levels.insert_into_level(
            1,
            fixture_segment(5, string_key_range("a", "g"), 128 * 1_024 * 1_024),
        );
        levels.insert_into_level(
            1,
            fixture_segment(6, string_key_range("a", "g"), 128 * 1_024 * 1_024),
        );
        levels.insert_into_level(
            1,
            fixture_segment(7, string_key_range("y", "z"), 128 * 1_024 * 1_024),
        );
        levels.insert_into_level(
            1,
            fixture_segment(8, string_key_range("y", "z"), 128 * 1_024 * 1_024),
        );

        assert_eq!(
            compactor.choose(&levels, &Config::default()),
            Choice::Merge(CompactionInput {
                dest_level: 1,
                segment_ids: vec![1, 2, 3, 4, 5, 6],
                target_size: 128 * 1_024 * 1_024
            })
        );

        levels.hide_segments(&[5]);
        assert_eq!(
            compactor.choose(&levels, &Config::default()),
            Choice::DoNothing
        );

        Ok(())
    }

    #[test]
    fn levelled_deeper_level_with_overlap() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let compactor = Strategy {
            target_size: 128 * 1_024 * 1_024,
            ..Default::default()
        };
        let config = Config::default().level_ratio(2);

        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;

        levels.insert_into_level(
            2,
            fixture_segment(4, string_key_range("f", "l"), 128 * 1_024 * 1_024),
        );
        assert_eq!(compactor.choose(&levels, &config), Choice::DoNothing);

        levels.insert_into_level(
            1,
            fixture_segment(1, string_key_range("a", "g"), 128 * 1_024 * 1_024),
        );
        assert_eq!(compactor.choose(&levels, &config), Choice::DoNothing);

        levels.insert_into_level(
            1,
            fixture_segment(2, string_key_range("h", "t"), 128 * 1_024 * 1_024),
        );

        levels.insert_into_level(
            1,
            fixture_segment(3, string_key_range("h", "t"), 128 * 1_024 * 1_024),
        );

        assert_eq!(
            compactor.choose(&levels, &config),
            Choice::Merge(CompactionInput {
                dest_level: 2,
                segment_ids: vec![1, 4],
                target_size: 128 * 1_024 * 1_024
            })
        );

        Ok(())
    }

    #[test]
    fn levelled_deeper_level_no_overlap() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let compactor = Strategy {
            target_size: 128 * 1_024 * 1_024,
            ..Default::default()
        };
        let config = Config::default().level_ratio(2);

        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;

        levels.insert_into_level(
            2,
            fixture_segment(4, string_key_range("k", "l"), 128 * 1_024 * 1_024),
        );
        assert_eq!(compactor.choose(&levels, &config), Choice::DoNothing);

        levels.insert_into_level(
            1,
            fixture_segment(1, string_key_range("a", "g"), 128 * 1_024 * 1_024),
        );
        assert_eq!(compactor.choose(&levels, &config), Choice::DoNothing);

        levels.insert_into_level(
            1,
            fixture_segment(2, string_key_range("h", "t"), 128 * 1_024 * 1_024),
        );

        levels.insert_into_level(
            1,
            fixture_segment(3, string_key_range("h", "t"), 128 * 1_024 * 1_024),
        );

        assert_eq!(
            compactor.choose(&levels, &config),
            Choice::Move(CompactionInput {
                dest_level: 2,
                segment_ids: vec![1],
                target_size: 128 * 1_024 * 1_024
            })
        );

        Ok(())
    }

    #[test]
    fn levelled_last_level_with_overlap() -> crate::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let compactor = Strategy {
            target_size: 128 * 1_024 * 1_024,
            ..Default::default()
        };
        let config = Config::default().level_ratio(2);

        let mut levels = LevelManifest::create_new(4, tempdir.path().join(LEVELS_MANIFEST_FILE))?;

        levels.insert_into_level(
            3,
            fixture_segment(5, string_key_range("f", "l"), 128 * 1_024 * 1_024),
        );
        assert_eq!(compactor.choose(&levels, &config), Choice::DoNothing);

        levels.insert_into_level(
            2,
            fixture_segment(1, string_key_range("a", "g"), 128 * 1_024 * 1_024),
        );
        assert_eq!(compactor.choose(&levels, &config), Choice::DoNothing);

        levels.insert_into_level(
            2,
            fixture_segment(2, string_key_range("a", "g"), 128 * 1_024 * 1_024),
        );
        assert_eq!(compactor.choose(&levels, &config), Choice::DoNothing);

        levels.insert_into_level(
            2,
            fixture_segment(3, string_key_range("a", "g"), 128 * 1_024 * 1_024),
        );
        assert_eq!(compactor.choose(&levels, &config), Choice::DoNothing);

        levels.insert_into_level(
            2,
            fixture_segment(4, string_key_range("a", "g"), 128 * 1_024 * 1_024),
        );

        levels.insert_into_level(
            2,
            fixture_segment(6, string_key_range("y", "z"), 128 * 1_024 * 1_024),
        );

        assert_eq!(
            compactor.choose(&levels, &config),
            Choice::Merge(CompactionInput {
                dest_level: 3,
                segment_ids: vec![1, 5],
                target_size: 128 * 1_024 * 1_024
            })
        );

        Ok(())
    }
}
