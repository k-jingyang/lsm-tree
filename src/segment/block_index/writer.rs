use super::{IndexBlock, KeyedBlockHandle};
use crate::{
    file::{BLOCKS_FILE, INDEX_BLOCKS_FILE, TOP_LEVEL_INDEX_FILE},
    segment::block::header::Header as BlockHeader,
    serde::Serializable,
    value::UserKey,
};
use std::{
    fs::{File, OpenOptions},
    io::{BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

fn concat_files<P: AsRef<Path>>(src_path: P, dest_path: P) -> crate::Result<()> {
    let reader = File::open(src_path)?;
    let mut reader = BufReader::new(reader);

    let writer = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dest_path)?;
    let mut writer = BufWriter::new(writer);

    std::io::copy(&mut reader, &mut writer)?;
    writer.flush()?;

    Ok(())
}

pub struct Writer {
    path: PathBuf,
    file_pos: u64,
    block_writer: Option<BufWriter<File>>,
    index_writer: BufWriter<File>,
    block_size: u32,
    block_counter: u32,
    block_handles: Vec<KeyedBlockHandle>,
    tli_pointers: Vec<KeyedBlockHandle>,
}

impl Writer {
    pub fn new<P: AsRef<Path>>(path: P, block_size: u32) -> crate::Result<Self> {
        let block_writer = File::create(path.as_ref().join(INDEX_BLOCKS_FILE))?;
        let block_writer = BufWriter::with_capacity(u16::MAX.into(), block_writer);

        let index_writer = File::create(path.as_ref().join(TOP_LEVEL_INDEX_FILE))?;
        let index_writer = BufWriter::new(index_writer);

        Ok(Self {
            path: path.as_ref().into(),
            file_pos: 0,
            block_writer: Some(block_writer),
            index_writer,
            block_counter: 0,
            block_size,
            block_handles: Vec::with_capacity(1_000),
            tli_pointers: Vec::with_capacity(1_000),
        })
    }

    fn write_block(&mut self) -> crate::Result<()> {
        let mut block_writer = self.block_writer.as_mut().expect("should exist");

        // Write to file
        let (header, data) = IndexBlock::to_bytes_compressed(&self.block_handles)?;

        header.serialize(&mut block_writer)?;
        block_writer.write_all(&data)?;

        let bytes_written = BlockHeader::serialized_len() + data.len();

        // Expect is fine, because the chunk is not empty
        let last = self
            .block_handles
            .last()
            .expect("Chunk should not be empty");

        let index_block_handle = KeyedBlockHandle {
            end_key: last.end_key.clone(),
            offset: self.file_pos,
        };

        self.tli_pointers.push(index_block_handle);

        self.block_counter = 0;
        self.file_pos += bytes_written as u64;

        self.block_handles.clear();

        Ok(())
    }

    pub fn register_block(&mut self, start_key: UserKey, offset: u64) -> crate::Result<()> {
        let block_handle_size = (start_key.len() + std::mem::size_of::<KeyedBlockHandle>()) as u32;

        let block_handle = KeyedBlockHandle {
            end_key: start_key,
            offset,
        };

        self.block_handles.push(block_handle);

        self.block_counter += block_handle_size;

        if self.block_counter >= self.block_size {
            self.write_block()?;
        }

        Ok(())
    }

    fn write_top_level_index(&mut self, block_file_size: u64) -> crate::Result<()> {
        // IMPORTANT: I hate this, but we need to drop the writer
        // so the file is closed
        // so it can be replaced when using Windows
        self.block_writer = None;

        concat_files(
            self.path.join(INDEX_BLOCKS_FILE),
            self.path.join(BLOCKS_FILE),
        )?;

        log::trace!("Concatted index blocks onto blocks file");

        for item in &mut self.tli_pointers {
            item.offset += block_file_size;
        }

        // Write to file
        let (header, data) = IndexBlock::to_bytes_compressed(&self.tli_pointers)?;

        header.serialize(&mut self.index_writer)?;
        self.index_writer.write_all(&data)?;

        let bytes_written = BlockHeader::serialized_len() + data.len();

        self.index_writer.flush()?;
        self.index_writer.get_mut().sync_all()?;

        log::trace!(
            "Written top level index to {}, with {} pointers ({} bytes)",
            self.path.join(TOP_LEVEL_INDEX_FILE).display(),
            self.tli_pointers.len(),
            bytes_written,
        );

        Ok(())
    }

    pub fn finish(&mut self, block_file_size: u64) -> crate::Result<()> {
        if self.block_counter > 0 {
            self.write_block()?;
        }

        self.block_writer.as_mut().expect("should exist").flush()?;
        self.block_writer
            .as_mut()
            .expect("should exist")
            .get_mut()
            .sync_all()?;

        self.write_top_level_index(block_file_size)?;

        // TODO: add test to make sure writer is deleting index_blocks
        std::fs::remove_file(self.path.join(INDEX_BLOCKS_FILE))?;

        Ok(())
    }
}
