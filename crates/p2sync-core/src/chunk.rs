use serde::{Deserialize, Serialize};

/// Default chunk size: 256 KB.
pub const DEFAULT_CHUNK_SIZE: usize = 256 * 1024;

/// A content-addressed chunk of file data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub hash: [u8; 32],
    pub data: Vec<u8>,
}

/// Hash and offset information for a chunk, without the data payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChunkMeta {
    pub index: usize,
    pub offset: u64,
    pub len: u32,
    pub hash: [u8; 32],
}

/// Split data into fixed-size chunks, returning metadata and chunks.
pub fn split(data: &[u8], chunk_size: usize) -> (Vec<ChunkMeta>, Vec<Chunk>) {
    let mut metas = Vec::new();
    let mut chunks = Vec::new();

    for (i, slice) in data.chunks(chunk_size).enumerate() {
        let hash = blake3::hash(slice);
        let hash_bytes = *hash.as_bytes();

        metas.push(ChunkMeta {
            index: i,
            offset: (i * chunk_size) as u64,
            len: slice.len() as u32,
            hash: hash_bytes,
        });

        chunks.push(Chunk {
            hash: hash_bytes,
            data: slice.to_vec(),
        });
    }

    (metas, chunks)
}

/// Compute chunk metadata without retaining data (for indexing).
pub fn metadata(data: &[u8], chunk_size: usize) -> Vec<ChunkMeta> {
    data.chunks(chunk_size)
        .enumerate()
        .map(|(i, slice)| {
            let hash = blake3::hash(slice);
            ChunkMeta {
                index: i,
                offset: (i * chunk_size) as u64,
                len: slice.len() as u32,
                hash: *hash.as_bytes(),
            }
        })
        .collect()
}

/// Compute the aggregate hash for a file from its chunk hashes.
pub fn file_hash(chunk_metas: &[ChunkMeta]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for meta in chunk_metas {
        hasher.update(&meta.hash);
    }
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CS: usize = DEFAULT_CHUNK_SIZE;

    #[test]
    fn empty_data_produces_no_chunks() {
        let (metas, chunks) = split(b"", CS);
        assert!(metas.is_empty());
        assert!(chunks.is_empty());
    }

    #[test]
    fn small_data_produces_single_chunk() {
        let data = b"hello world";
        let (metas, chunks) = split(data, CS);

        assert_eq!(metas.len(), 1);
        assert_eq!(chunks.len(), 1);
        assert_eq!(metas[0].index, 0);
        assert_eq!(metas[0].offset, 0);
        assert_eq!(metas[0].len, data.len() as u32);
        assert_eq!(chunks[0].data, data.to_vec());
        assert_eq!(metas[0].hash, chunks[0].hash);
    }

    #[test]
    fn data_splits_at_chunk_boundary() {
        let data = vec![0xAB; CS * 3];
        let (metas, chunks) = split(&data, CS);

        assert_eq!(metas.len(), 3);
        assert_eq!(chunks.len(), 3);

        for (i, meta) in metas.iter().enumerate() {
            assert_eq!(meta.index, i);
            assert_eq!(meta.offset, (i * CS) as u64);
            assert_eq!(meta.len, CS as u32);
        }
    }

    #[test]
    fn data_with_remainder_chunk() {
        let data = vec![0xCD; CS * 2 + 100];
        let (metas, chunks) = split(&data, CS);

        assert_eq!(metas.len(), 3);
        assert_eq!(metas[2].len, 100);
        assert_eq!(chunks[2].data.len(), 100);
    }

    #[test]
    fn chunk_hash_matches_blake3() {
        let data = b"test data for hashing";
        let expected = *blake3::hash(data).as_bytes();
        let (metas, chunks) = split(data, CS);

        assert_eq!(metas[0].hash, expected);
        assert_eq!(chunks[0].hash, expected);
    }

    #[test]
    fn metadata_matches_split() {
        let data = vec![0xFF; CS * 2 + 500];
        let (metas_from_split, _) = split(&data, CS);
        let metas_from_fn = metadata(&data, CS);

        assert_eq!(metas_from_split, metas_from_fn);
    }

    #[test]
    fn file_hash_is_deterministic() {
        let data = vec![0x42; CS + 1];
        let metas = metadata(&data, CS);
        let h1 = file_hash(&metas);
        let h2 = file_hash(&metas);
        assert_eq!(h1, h2);
    }

    #[test]
    fn file_hash_changes_with_content() {
        let data_a = vec![0x00; CS + 1];
        let data_b = vec![0x01; CS + 1];
        let h_a = file_hash(&metadata(&data_a, CS));
        let h_b = file_hash(&metadata(&data_b, CS));
        assert_ne!(h_a, h_b);
    }

    #[test]
    fn reassembled_chunks_match_original() {
        let data = vec![0xDE; CS * 3 + 42];
        let (_, chunks) = split(&data, CS);
        let reassembled: Vec<u8> = chunks.iter().flat_map(|c| c.data.iter().copied()).collect();
        assert_eq!(reassembled, data);
    }

    #[test]
    fn custom_chunk_size() {
        let data = vec![0xAA; 1000];
        let (metas, _) = split(&data, 300);
        assert_eq!(metas.len(), 4); // 300+300+300+100
        assert_eq!(metas[3].len, 100);
    }
}
