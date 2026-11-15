use std::{
    collections::HashSet,
    io::{self, Cursor, Read, Write},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use flate2::{Compression, Decompress, FlushDecompress, Status, write::ZlibEncoder};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    effects::{NodeId, PipelineId},
    project::{Composition, CompositionId, MediaId, Project},
};

pub const MAGIC: &[u8; 8] = b"KAMA\r\n\x1a\n";
pub const MAX_FILE_SIZE: u64 = 1024 * 1024 * 1024;
pub const MAX_DECODED_SIZE: u64 = MAX_FILE_SIZE;
const MAX_SECTION_SIZE: u64 = 256 * 1024 * 1024;
const CONTAINER_VERSION: u32 = 1;
const HEADER_SIZE: usize = 80;
const ENTRY_SIZE: usize = 40;
const MAX_SECTIONS: u32 = 4096;
const REQUIRED: u32 = 1;

#[derive(Clone, Copy, Debug, Default)]
pub struct SnapshotMetadata {
    project_id: [u8; 16],
    generation: u64,
    saved_unix_ms: u64,
}

impl SnapshotMetadata {
    pub fn next(self) -> Result<Self> {
        let mut project_id = self.project_id;
        if project_id == [0; 16] {
            getrandom::fill(&mut project_id)
                .map_err(|error| anyhow::anyhow!("generate project identity: {error}"))?;
            ensure!(project_id != [0; 16], "generated an empty project identity");
        }
        Ok(Self {
            project_id,
            generation: self
                .generation
                .checked_add(1)
                .context("save generation exhausted")?,
            saved_unix_ms: u64::try_from(
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
            )?,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct ProjectMetadata {
    format_version: u32,
    name: String,
    next_media_id: MediaId,
    next_pipeline_id: PipelineId,
    next_node_id: NodeId,
    next_composition_id: CompositionId,
}

#[derive(Serialize, Deserialize)]
struct CompositionRecord {
    id: CompositionId,
    name: String,
    settings: crate::project::CompositionSettings,
    timeline: crate::timeline::TimelineDocument,
}

#[derive(Serialize)]
struct CompositionRef<'a> {
    id: CompositionId,
    name: &'a str,
    settings: &'a crate::project::CompositionSettings,
    timeline: &'a crate::timeline::TimelineDocument,
}

struct Section {
    tag: [u8; 4],
    codec: u16,
    decoded_size: u64,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct SectionBuffer(Vec<u8>);

impl Write for SectionBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let size = self
            .0
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("project section size overflow"))?;
        if u64::try_from(size).map_err(io::Error::other)? > MAX_SECTION_SIZE {
            return Err(io::Error::other("project section exceeds 256 MiB"));
        }
        self.0.try_reserve(bytes.len()).map_err(io::Error::other)?;
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Section {
    fn encode(tag: [u8; 4], value: &impl Serialize) -> Result<Self> {
        let mut buffer = SectionBuffer::default();
        ciborium::ser::into_writer(value, &mut buffer).context("encode project section")?;
        let raw = buffer.0;
        let decoded_size = u64::try_from(raw.len())?;
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&raw)?;
        let compressed = encoder.finish()?;
        let (codec, bytes) = if compressed.len() < raw.len() {
            (1, compressed)
        } else {
            (0, raw)
        };
        Ok(Self {
            tag,
            codec,
            decoded_size,
            bytes,
        })
    }
}

pub fn encode(project: &Project, snapshot: SnapshotMetadata) -> Result<Vec<u8>> {
    let metadata = ProjectMetadata {
        format_version: project.format_version,
        name: project.name.clone(),
        next_media_id: project.next_media_id,
        next_pipeline_id: project.next_pipeline_id,
        next_node_id: project.next_node_id,
        next_composition_id: project.next_composition_id,
    };
    let compositions = project
        .compositions
        .iter()
        .map(|composition| CompositionRef {
            id: composition.id,
            name: &composition.name,
            settings: &composition.settings,
            timeline: &composition.timeline,
        })
        .collect::<Vec<_>>();
    let sections = [
        Section::encode(*b"META", &metadata)?,
        Section::encode(*b"MEDI", &project.media)?,
        Section::encode(*b"PIPE", &project.pipelines)?,
        Section::encode(*b"COMP", &compositions)?,
    ];
    let directory_size = sections
        .len()
        .checked_mul(ENTRY_SIZE)
        .context("directory size overflow")?;
    let mut offset = u64::try_from(
        HEADER_SIZE
            .checked_add(directory_size)
            .context("header size overflow")?,
    )?;
    let mut directory = Vec::with_capacity(directory_size);
    let mut total_decoded = 0u64;
    for section in &sections {
        let stored_size = u64::try_from(section.bytes.len())?;
        directory.extend_from_slice(&section.tag);
        directory.extend_from_slice(&1u16.to_le_bytes());
        directory.extend_from_slice(&section.codec.to_le_bytes());
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(&stored_size.to_le_bytes());
        directory.extend_from_slice(&section.decoded_size.to_le_bytes());
        directory.extend_from_slice(&crc32fast::hash(&section.bytes).to_le_bytes());
        directory.extend_from_slice(&REQUIRED.to_le_bytes());
        offset = offset
            .checked_add(stored_size)
            .context("project size overflow")?;
        total_decoded = total_decoded
            .checked_add(section.decoded_size)
            .context("decoded size overflow")?;
    }
    ensure!(
        offset <= MAX_FILE_SIZE && total_decoded <= MAX_DECODED_SIZE,
        "project exceeds 1 GiB limit"
    );
    let mut output = Vec::with_capacity(usize::try_from(offset)?);
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&CONTAINER_VERSION.to_le_bytes());
    output.extend_from_slice(&u32::try_from(HEADER_SIZE)?.to_le_bytes());
    output.extend_from_slice(&0u32.to_le_bytes());
    output.extend_from_slice(&u32::try_from(sections.len())?.to_le_bytes());
    output.extend_from_slice(&offset.to_le_bytes());
    output.extend_from_slice(&snapshot.project_id);
    output.extend_from_slice(&snapshot.generation.to_le_bytes());
    output.extend_from_slice(&snapshot.saved_unix_ms.to_le_bytes());
    output.extend_from_slice(&0u64.to_le_bytes());
    output.extend_from_slice(&crc32fast::hash(&directory).to_le_bytes());
    output.extend_from_slice(&crc32fast::hash(&output).to_le_bytes());
    output.extend_from_slice(&directory);
    for section in sections {
        output.extend_from_slice(&section.bytes);
    }
    Ok(output)
}

struct DirectoryEntry {
    tag: [u8; 4],
    version: u16,
    codec: u16,
    offset: u64,
    stored_size: u64,
    decoded_size: u64,
    checksum: u32,
    flags: u32,
}

impl DirectoryEntry {
    fn read(reader: &mut impl Read) -> Result<Self> {
        Ok(Self {
            tag: read_array(reader)?,
            version: u16::from_le_bytes(read_array(reader)?),
            codec: u16::from_le_bytes(read_array(reader)?),
            offset: u64::from_le_bytes(read_array(reader)?),
            stored_size: u64::from_le_bytes(read_array(reader)?),
            decoded_size: u64::from_le_bytes(read_array(reader)?),
            checksum: u32::from_le_bytes(read_array(reader)?),
            flags: u32::from_le_bytes(read_array(reader)?),
        })
    }

    fn stored_bytes<'a>(&self, data: &'a [u8]) -> Result<&'a [u8]> {
        let start = usize::try_from(self.offset)?;
        let end = usize::try_from(
            self.offset
                .checked_add(self.stored_size)
                .context("section offset overflow")?,
        )?;
        let stored = data.get(start..end).context("truncated project section")?;
        ensure!(
            crc32fast::hash(stored) == self.checksum,
            "project section checksum mismatch"
        );
        Ok(stored)
    }

    fn decode(&self, data: &[u8]) -> Result<Vec<u8>> {
        let stored = self.stored_bytes(data)?;
        let raw = match self.codec {
            0 => stored.to_vec(),
            1 => {
                let mut decoder = Decompress::new(true);
                let mut raw = Vec::new();
                raw.try_reserve_exact(usize::try_from(
                    self.decoded_size
                        .checked_add(1)
                        .context("decoded section size overflow")?,
                )?)
                .context("allocate decoded project section")?;
                let status = decoder
                    .decompress_vec(stored, &mut raw, FlushDecompress::Finish)
                    .context("decompress project section")?;
                ensure!(
                    status == Status::StreamEnd,
                    "incomplete compressed section or decoded size exceeded"
                );
                ensure!(
                    decoder.total_in() == u64::try_from(stored.len())?,
                    "trailing compressed section data"
                );
                raw
            }
            codec => anyhow::bail!("unsupported section compression {codec}"),
        };
        ensure!(
            u64::try_from(raw.len())? == self.decoded_size,
            "project section decoded size mismatch"
        );
        Ok(raw)
    }
}

pub fn decode(data: &[u8]) -> Result<Project> {
    ensure!(
        u64::try_from(data.len())? <= MAX_FILE_SIZE,
        "project exceeds 1 GiB limit"
    );
    let header = data.get(..HEADER_SIZE).context("truncated .kama header")?;
    let checksummed_header = header.get(..76).context("truncated .kama header")?;
    let mut reader = Cursor::new(header);
    ensure!(
        &read_array::<8>(&mut reader)? == MAGIC,
        "invalid .kama signature"
    );
    let version = u32::from_le_bytes(read_array(&mut reader)?);
    ensure!(
        version == CONTAINER_VERSION,
        "unsupported .kama container version {version}"
    );
    ensure!(
        u32::from_le_bytes(read_array(&mut reader)?) == u32::try_from(HEADER_SIZE)?,
        "invalid .kama header size"
    );
    let flags = u32::from_le_bytes(read_array(&mut reader)?);
    ensure!(
        flags == 0,
        "unsupported .kama snapshot flags {flags:#x}; recovery snapshots cannot be opened as saved projects"
    );
    let section_count = u32::from_le_bytes(read_array(&mut reader)?);
    ensure!(
        (4..=MAX_SECTIONS).contains(&section_count),
        "invalid project section count"
    );
    let file_size = u64::from_le_bytes(read_array(&mut reader)?);
    ensure!(
        file_size == u64::try_from(data.len())?,
        "project file size mismatch"
    );
    let snapshot = SnapshotMetadata {
        project_id: read_array(&mut reader)?,
        generation: u64::from_le_bytes(read_array(&mut reader)?),
        saved_unix_ms: u64::from_le_bytes(read_array(&mut reader)?),
    };
    ensure!(
        snapshot.project_id != [0; 16] && snapshot.generation != 0,
        "invalid project snapshot identity"
    );
    ensure!(
        u64::from_le_bytes(read_array(&mut reader)?) == 0,
        "unsupported required .kama features"
    );
    let directory_checksum = u32::from_le_bytes(read_array(&mut reader)?);
    let header_checksum = u32::from_le_bytes(read_array(&mut reader)?);
    ensure!(
        crc32fast::hash(checksummed_header) == header_checksum,
        "project header checksum mismatch"
    );
    let directory_end = HEADER_SIZE
        .checked_add(
            usize::try_from(section_count)?
                .checked_mul(ENTRY_SIZE)
                .context("directory size overflow")?,
        )
        .context("directory offset overflow")?;
    let directory = data
        .get(HEADER_SIZE..directory_end)
        .context("truncated project directory")?;
    ensure!(
        crc32fast::hash(directory) == directory_checksum,
        "project directory checksum mismatch"
    );
    let mut directory_reader = Cursor::new(directory);
    let mut entries = Vec::with_capacity(usize::try_from(section_count)?);
    let mut tags = HashSet::new();
    let mut expected_offset = u64::try_from(directory_end)?;
    let mut total_decoded = 0u64;
    for _ in 0..section_count {
        let entry = DirectoryEntry::read(&mut directory_reader)?;
        ensure!(tags.insert(entry.tag), "duplicate project section");
        ensure!(entry.flags & !REQUIRED == 0, "unsupported section flags");
        ensure!(
            entry.offset == expected_offset,
            "overlapping or noncontiguous project sections"
        );
        ensure!(
            entry.stored_size > 0
                && entry.stored_size <= MAX_SECTION_SIZE
                && entry.decoded_size > 0
                && entry.decoded_size <= MAX_SECTION_SIZE,
            "invalid project section size"
        );
        expected_offset = expected_offset
            .checked_add(entry.stored_size)
            .context("section offset overflow")?;
        ensure!(expected_offset <= file_size, "truncated project section");
        total_decoded = total_decoded
            .checked_add(entry.decoded_size)
            .context("decoded size overflow")?;
        ensure!(
            total_decoded <= MAX_DECODED_SIZE,
            "decoded project exceeds 1 GiB limit"
        );
        let known = matches!(&entry.tag, b"META" | b"MEDI" | b"PIPE" | b"COMP");
        ensure!(
            known || entry.flags & REQUIRED == 0,
            "unsupported required project section"
        );
        if known {
            ensure!(
                entry.version == 1 && entry.flags == REQUIRED,
                "unsupported project section version or flags"
            );
        }
        entries.push(entry);
    }
    ensure!(expected_offset == file_size, "trailing project data");
    let mut metadata = None;
    let mut media = None;
    let mut pipelines = None;
    let mut compositions = None;
    for entry in entries {
        if !matches!(&entry.tag, b"META" | b"MEDI" | b"PIPE" | b"COMP") {
            entry.stored_bytes(data)?;
            continue;
        }
        let raw = entry
            .decode(data)
            .with_context(|| format!("read section {}", String::from_utf8_lossy(&entry.tag)))?;
        match &entry.tag {
            b"META" => metadata = Some(decode_cbor::<ProjectMetadata>(&raw)?),
            b"MEDI" => media = Some(decode_cbor(&raw)?),
            b"PIPE" => pipelines = Some(decode_cbor(&raw)?),
            b"COMP" => compositions = Some(decode_cbor::<Vec<CompositionRecord>>(&raw)?),
            _ => {}
        }
    }
    let metadata = metadata.context("missing project metadata section")?;
    Ok(Project {
        format_version: metadata.format_version,
        name: metadata.name,
        media: media.context("missing project media section")?,
        pipelines: pipelines.context("missing project pipelines section")?,
        compositions: compositions
            .context("missing project compositions section")?
            .into_iter()
            .map(|record| Composition {
                id: record.id,
                name: record.name,
                settings: record.settings,
                timeline: record.timeline,
            })
            .collect(),
        next_media_id: metadata.next_media_id,
        next_pipeline_id: metadata.next_pipeline_id,
        next_node_id: metadata.next_node_id,
        next_composition_id: metadata.next_composition_id,
        active_composition: 0,
        storage: Arc::new(Mutex::new(snapshot)),
    })
}

pub fn decode_cbor<T: DeserializeOwned>(data: &[u8]) -> Result<T> {
    let mut reader = Cursor::new(data);
    let value = ciborium::de::from_reader(&mut reader).context("decode project CBOR")?;
    ensure!(
        reader.position() == u64::try_from(data.len())?,
        "trailing project CBOR data"
    );
    Ok(value)
}

fn read_array<const N: usize>(reader: &mut impl Read) -> Result<[u8; N]> {
    let mut bytes = [0; N];
    reader
        .read_exact(&mut bytes)
        .context("truncated project structure")?;
    Ok(bytes)
}
