use crate::splitter::Splitter;
use fixedbitset::FixedBitSet;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use aws_sdk_s3::{Client, primitives::ByteStream};
use aws_config;

#[derive(Clone)]
pub struct Interner {
    version: usize,
    vocabulary: Vec<String>,
    prefix_to_completions: HashMap<Vec<usize>, FixedBitSet>,
}

// Custom Encode/Decode for Interner
impl bincode::Encode for Interner {
    fn encode<E: bincode::enc::Encoder>(&self, encoder: &mut E) -> Result<(), bincode::error::EncodeError> {
        self.version.encode(encoder)?;
        self.vocabulary.encode(encoder)?;
        // Serialize prefix_to_completions as Vec<(Vec<usize>, Vec<u32>)>
        let prefix_vec: Vec<(Vec<usize>, Vec<u32>)> = self.prefix_to_completions.iter().map(|(k, v)| {
            (k.clone(), v.ones().map(|x| x as u32).collect())
        }).collect();
        prefix_vec.encode(encoder)?;
        Ok(())
    }
}

impl<Context> bincode::Decode<Context> for Interner {
    fn decode<D: bincode::de::Decoder>(decoder: &mut D) -> Result<Self, bincode::error::DecodeError> {
        let version = usize::decode(decoder)?;
        let vocabulary = Vec::<String>::decode(decoder)?;
        let prefix_vec = Vec::<(Vec<usize>, Vec<u32>)>::decode(decoder)?;
        let mut prefix_to_completions = HashMap::new();
        let vocab_len = vocabulary.len();
        for (prefix, completions) in prefix_vec {
            let mut fbs = FixedBitSet::with_capacity(vocab_len);
            fbs.grow(vocab_len);
            for idx in completions {
                fbs.insert(idx as usize);
            }
            prefix_to_completions.insert(prefix, fbs);
        }
        Ok(Interner {
            version,
            vocabulary,
            prefix_to_completions,
        })
    }
}

impl Interner {
    fn from_text(text: &str) -> Self {
        let splitter = Splitter::new();
        let vocab = splitter.vocabulary(text);
        let phrases = splitter.phrases(text);
        let mut vocabulary = Vec::new();
        for word in &vocab {
            if !vocabulary.contains(word) {
                vocabulary.push(word.clone());
            }
        }
        let new_vocab_len = vocabulary.len();
        let prefix_to_completions = Self::build_prefix_to_completions(&phrases, &vocabulary, new_vocab_len, None);
        Interner {
            version: 1,
            vocabulary,
            prefix_to_completions,
        }
    }

    fn add_text(&self, text: &str) -> Self {
        if text.trim().is_empty() {
            return Interner {
                version: self.version + 1,
                vocabulary: self.vocabulary.clone(),
                prefix_to_completions: self.prefix_to_completions.clone(),
            };
        }
        let splitter = Splitter::new();
        let vocab = splitter.vocabulary(text);
        let phrases = splitter.phrases(text);

        let mut vocabulary = self.vocabulary.clone();
        for word in &vocab {
            if !vocabulary.contains(word) {
                vocabulary.push(word.clone());
            }
        }
        let new_vocab_len = vocabulary.len();

        let prefix_to_completions = Self::build_prefix_to_completions(
            &phrases,
            &vocabulary,
            new_vocab_len,
            Some(&self.prefix_to_completions),
        );

        Interner {
            version: self.version + 1,
            vocabulary,
            prefix_to_completions,
        }
    }

    fn build_prefix_to_completions(
        phrases: &[Vec<String>],
        vocabulary: &[String],
        vocab_len: usize,
        existing: Option<&HashMap<Vec<usize>, FixedBitSet>>,
    ) -> HashMap<Vec<usize>, FixedBitSet> {
        let mut prefix_to_completions = match existing {
            Some(map) => {
                let mut new_map = map.clone();
                for bitset in new_map.values_mut() {
                    bitset.grow(vocab_len);
                }
                new_map
            }
            None => HashMap::new(),
        };

        for phrase in phrases {
            if phrase.len() < 2 {
                continue;
            }
            let indices: Vec<usize> = phrase
                .iter()
                .map(|word| {
                    vocabulary
                        .iter()
                        .position(|v| v == word)
                        .expect("Word should be in vocabulary")
                })
                .collect();
            let prefix = indices[..indices.len() - 1].to_vec();
            let completion_word_index = indices[indices.len() - 1];
            if completion_word_index < vocab_len {
                let bitset = prefix_to_completions.entry(prefix).or_insert_with(|| {
                    let mut fbs = FixedBitSet::with_capacity(vocab_len);
                    fbs.grow(vocab_len);
                    fbs
                });
                bitset.insert(completion_word_index);
            }
        }
        // Ensure every vocabulary item is present as a prefix key
        for idx in 0..vocab_len {
            let prefix = vec![idx];
            if !prefix_to_completions.contains_key(&prefix) {
                let mut fbs = FixedBitSet::with_capacity(vocab_len);
                fbs.grow(vocab_len);
                prefix_to_completions.insert(prefix, fbs);
            }
        }
        prefix_to_completions
    }

    pub fn version(&self) -> usize {
        self.version
    }

    pub fn vocabulary(&self) -> &[String] {
        &self.vocabulary
    }

    pub fn string_for_index(&self, index: usize) -> &str {
        self.vocabulary
            .get(index)
            .map(|s| s.as_str())
            .expect("Index out of bounds in Interner::string_for_index")
    }

    pub fn completions_for_prefix(&self, prefix: &Vec<usize>) -> Option<&FixedBitSet> {
        self.prefix_to_completions.get(prefix)
    }

    fn get_required_bits(&self, required: &[Vec<usize>]) -> FixedBitSet {
        let mut result = FixedBitSet::with_capacity(self.vocabulary.len());
        result.grow(self.vocabulary.len());
        if required.is_empty() {
            result.set_range(.., true);
            return result;
        }
        let mut first = true;
        for prefix in required {
            let bitset = self
                .prefix_to_completions
                .get(prefix)
                .unwrap_or_else(|| panic!("Required prefix {:?} not found in prefix_to_completions", prefix));
            if first {
                result.clone_from(bitset);
                first = false;
            } else {
                result.intersect_with(bitset);
            }
        }
        result
    }

    fn get_forbidden_bits(&self, forbidden: &[usize]) -> FixedBitSet {
        let mut bitset = FixedBitSet::with_capacity(self.vocabulary.len());
        bitset.grow(self.vocabulary.len());
        if forbidden.is_empty() {
            bitset.set_range(.., true);
            return bitset;
        }
        bitset.set_range(.., true);
        for &idx in forbidden {
            bitset.set(idx, false);
        }
        bitset
    }

    pub fn intersect(&self, required: &[Vec<usize>], forbidden: &[usize]) -> Vec<usize> {
        let required_bits = self.get_required_bits(required);
        let forbidden_bits = self.get_forbidden_bits(forbidden);
        let mut intersection = required_bits.clone();
        intersection.intersect_with(&forbidden_bits);
        intersection.ones().collect()
    }
}

pub trait InternerHolderLike {
    fn get(&self, version: usize) -> Option<Interner>;
    fn latest_version(&self) -> usize;
    fn get_latest(&self) -> Option<Interner>;
    fn versions(&self) -> Vec<usize>;
    fn delete(&mut self, version: usize);
    fn add_text_with_seed<Q: crate::queue::QueueLike>(&mut self, text: &str, workq: &mut Q);
    fn with_seed<Q: crate::queue::QueueLike>(text: &str, workq: &mut Q) -> Self where Self: Sized;
}

pub struct InMemoryInternerHolder {
    pub interners: std::collections::HashMap<usize, Interner>,
}

impl InMemoryInternerHolder {
    fn from_text_internal(text: &str) -> Self {
        let interner = Interner::from_text(text);
        let mut interners = std::collections::HashMap::new();
        interners.insert(interner.version(), interner);
        InMemoryInternerHolder { interners }
    }
}

impl InternerHolderLike for InMemoryInternerHolder {
    fn get(&self, version: usize) -> Option<Interner> {
        self.interners.get(&version).cloned()
    }
    
    fn latest_version(&self) -> usize {
        *self.interners.keys().max().unwrap_or(&0)
    }
    fn get_latest(&self) -> Option<Interner> {
        let v = self.latest_version();
        self.get(v)
    }
    fn versions(&self) -> Vec<usize> {
        let mut versions: Vec<usize> = self.interners.keys().cloned().collect();
        versions.sort_unstable();
        versions
    }
    fn delete(&mut self, version: usize) {
        self.interners.remove(&version);
    }
    fn add_text_with_seed<Q: crate::queue::QueueLike>(&mut self, text: &str, workq: &mut Q) {
        let latest = self.interners.values().max_by_key(|i| i.version()).unwrap();
        let interner = latest.add_text(text);
        self.interners.insert(interner.version(), interner.clone());
        let version = interner.version();
        let ortho_seed = crate::ortho::Ortho::new(version);
        println!("[interner] Seeding workq with ortho: id={}, version={}, dims={:?}", ortho_seed.id(), version, ortho_seed.dims());
        workq.push_many(vec![ortho_seed]);
    }
    
    fn with_seed<Q: crate::queue::QueueLike>(text: &str, workq: &mut Q) -> Self {
        let holder = InMemoryInternerHolder::from_text_internal(text);
        let version = holder.latest_version();
        let ortho_seed = crate::ortho::Ortho::new(version);
        println!("[interner] Seeding workq with ortho: id={}, version={}, dims={:?}", ortho_seed.id(), version, ortho_seed.dims());
        workq.push_many(vec![ortho_seed]);
        holder
    }
}

pub struct FileInternerHolder {
    dir: PathBuf,
}

impl FileInternerHolder {
    fn new_internal(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        fs::create_dir_all(&dir).ok();
        Self { dir }
    }
    fn file_path(&self, version: usize) -> PathBuf {
        self.dir.join(format!("interner_{}.bin", version))
    }
    fn load_interner(&self, version: usize) -> Option<Interner> {
        let path = self.file_path(version);
        if let Ok(data) = fs::read(path) {
            bincode::decode_from_slice(&data, bincode::config::standard()).ok().map(|(v, _)| v)
        } else {
            None
        }
    }
    fn put(&mut self, interner: Interner) {
        let path = self.file_path(interner.version());
        let data = bincode::encode_to_vec(&interner, bincode::config::standard()).expect("Failed to serialize Interner");
        fs::write(path, data).expect("Failed to write Interner file");
    }
}

impl InternerHolderLike for FileInternerHolder {
    fn get(&self, version: usize) -> Option<Interner> {
        self.load_interner(version)
    }

    fn latest_version(&self) -> usize {
        self.versions().into_iter().max().unwrap_or(0)
    }
    fn get_latest(&self) -> Option<Interner> {
        let v = self.latest_version();
        self.get(v)
    }
    fn versions(&self) -> Vec<usize> {
        let mut versions = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                if let Some(fname) = entry.file_name().to_str() {
                    if let Some(vstr) = fname.strip_prefix("interner_").and_then(|s| s.strip_suffix(".bin")) {
                        if let Ok(v) = vstr.parse::<usize>() {
                            versions.push(v);
                        }
                    }
                }
            }
        }
        versions.sort_unstable();
        versions
    }
    fn delete(&mut self, version: usize) {
        let path = self.file_path(version);
        let _ = fs::remove_file(path);
    }
    fn add_text_with_seed<Q: crate::queue::QueueLike>(&mut self, text: &str, workq: &mut Q) {
        let latest = self.get_latest().unwrap();
        let interner = latest.add_text(text);
        self.put(interner.clone());
        let version = interner.version();
        let ortho_seed = crate::ortho::Ortho::new(version);
        println!("[interner] Seeding workq with ortho: id={}, version={}, dims={:?}", ortho_seed.id(), version, ortho_seed.dims());
        workq.push_many(vec![ortho_seed]);
    }
    
    fn with_seed<Q: crate::queue::QueueLike>(text: &str, workq: &mut Q) -> Self {
        let dir = std::env::var("INTERNER_FILE_LOCATION")
            .expect("INTERNER_FILE_LOCATION not set in environment. Please set it in your .env file.");
        let mut holder = FileInternerHolder::new_internal(dir);
        let interner = Interner::from_text(text);
        holder.put(interner.clone());
        let version = interner.version();
        let ortho_seed = crate::ortho::Ortho::new(version);
        println!("[interner] Seeding workq with ortho: id={}, version={}, dims={:?}", ortho_seed.id(), version, ortho_seed.dims());
        workq.push_many(vec![ortho_seed]);
        holder
    }
}

pub struct BlobInternerHolder {
    client: Client,
    bucket: String,
    rt: std::sync::Arc<tokio::runtime::Runtime>,
}

impl BlobInternerHolder {
    pub fn new_internal() -> Self {
        let bucket = std::env::var("FOLD_INTERNER_BLOB_BUCKET").unwrap_or_else(|_| "internerdata".to_string());
        let endpoint_url = std::env::var("FOLD_INTERNER_BLOB_ENDPOINT").unwrap_or_else(|_| "http://minio:9000".to_string());
        let access_key = std::env::var("FOLD_INTERNER_BLOB_ACCESS_KEY").unwrap_or_else(|_| "minioadmin".to_string());
        let secret_key = std::env::var("FOLD_INTERNER_BLOB_SECRET_KEY").unwrap_or_else(|_| "minioadmin".to_string());
        let region = "us-east-1";

        let rt = std::sync::Arc::new(tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime"));
        let config = rt.block_on(async {
            aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(region)
                .endpoint_url(&endpoint_url)
                .credentials_provider(aws_sdk_s3::config::Credentials::new(
                    &access_key,
                    &secret_key,
                    None,
                    None,
                    "minio",
                ))
                .load()
                .await
        });
        let s3_config = aws_sdk_s3::config::Builder::from(&config)
            .force_path_style(true)
            .build();
        let client = Client::from_conf(s3_config);
        // Create the bucket if it does not exist
        let bucket_clone = bucket.clone();
        rt.block_on(async {
            let head_result = client.head_bucket().bucket(&bucket_clone).send().await;
            if head_result.is_err() {
                let _ = client.create_bucket().bucket(&bucket_clone).send().await;
            }
        });
        Self { client, bucket, rt }
    }
    fn put_blocking(&self, key: &str, data: &[u8]) {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        let data = data.to_vec();
        self.rt.block_on(async move {
            let result = client.put_object()
                .bucket(&bucket)
                .key(key)
                .body(ByteStream::from(data))
                .send()
                .await;
            result.unwrap()
        });
    }
    fn get_blocking(&self, key: &str) -> Option<Vec<u8>> {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        self.rt.block_on(async move {
            match client.get_object().bucket(&bucket).key(key).send().await {
                Ok(resp) => {
                    let data = resp.body.collect().await.ok()?;
                    let bytes = data.into_bytes();
                    Some(bytes.to_vec())
                },
                Err(_e) => {
                    None
                },
            }
        })
    }
    fn list_blocking(&self) -> Vec<String> {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        self.rt.block_on(async move {
            match client.list_objects_v2().bucket(&bucket).send().await {
                Ok(resp) => {
                    let keys: Vec<String> = resp.contents().iter().filter_map(|obj| obj.key().map(|k| k.to_string())).collect();
                    keys
                },
                Err(_e) => {
                    vec![]
                },
            }
        })
    }
    fn delete_blocking(&self, key: &str) {
        let client = self.client.clone();
        let bucket = self.bucket.clone();
        self.rt.block_on(async move {
            let _ = client.delete_object().bucket(&bucket).key(key).send().await;
        });
    }
}

impl InternerHolderLike for BlobInternerHolder {
    fn get(&self, version: usize) -> Option<Interner> {
        let key = version.to_string();
        let result = self.get_blocking(&key).and_then(|data| {
            bincode::decode_from_slice::<Interner, _>(&data, bincode::config::standard()).ok().map(|(v, _)| v)
        });
        result
    }
    fn latest_version(&self) -> usize {
        let versions = self.list_blocking().iter().filter_map(|key| key.parse::<usize>().ok()).collect::<Vec<_>>();
        versions.into_iter().max().unwrap_or(0)
    }
    fn get_latest(&self) -> Option<Interner> {
        let v = self.latest_version();
        self.get(v)
    }
    fn versions(&self) -> Vec<usize> {
        self.list_blocking().iter().filter_map(|key| key.parse::<usize>().ok()).collect()
    }
    fn delete(&mut self, version: usize) {
        let key = version.to_string();
        self.delete_blocking(&key);
    }
    fn add_text_with_seed<Q: crate::queue::QueueLike>(&mut self, text: &str, workq: &mut Q) {
        let latest = self.get_latest().unwrap();    
        let interner = latest.add_text(text);
        let key = interner.version().to_string();
        let data = bincode::encode_to_vec(&interner, bincode::config::standard()).expect("Failed to serialize Interner");
        self.put_blocking(&key, &data);
        let version = interner.version();
        let ortho_seed = crate::ortho::Ortho::new(version);
        println!("[interner] Seeding workq with ortho: id={}, version={}, dims={:?}", ortho_seed.id(), version, ortho_seed.dims());
        workq.push_many(vec![ortho_seed]);
    }
    fn with_seed<Q: crate::queue::QueueLike>(text: &str, workq: &mut Q) -> Self {
        let holder = BlobInternerHolder::new_internal();
        let interner = Interner::from_text(text);
        let key = interner.version().to_string();
        let data = bincode::encode_to_vec(&interner, bincode::config::standard()).expect("Failed to serialize Interner");
        holder.put_blocking(&key, &data);
        let version = interner.version();
        let ortho_seed = crate::ortho::Ortho::new(version);
        println!("[interner] Seeding workq with ortho: id={}, version={}, dims={:?}", ortho_seed.id(), version, ortho_seed.dims());
        workq.push_many(vec![ortho_seed]);
        holder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fixedbitset::FixedBitSet;

    #[test]
    fn test_from_text_creates_interner() {
        let holder = InMemoryInternerHolder::with_seed("hello world", &mut crate::queue::MockQueue::new());
        let interner = holder.get(holder.latest_version()).unwrap();
        assert_eq!(interner.version(), 1);
        assert_eq!(interner.vocabulary.len(), 2);
        assert_eq!(interner.prefix_to_completions.len(), 2);
    }
    #[test]
    fn test_add_increments_version() {
        let holder = InMemoryInternerHolder::with_seed("hello world", &mut crate::queue::MockQueue::new());
        let interner = holder.get(holder.latest_version()).unwrap();
        let interner2 = interner.add_text("test");
        assert_eq!(interner2.version(), 2);
    }
    #[test]
    fn test_add_extends_vocabulary() {
        let holder = InMemoryInternerHolder::with_seed("hello world", &mut crate::queue::MockQueue::new());
        let interner = holder.get(holder.latest_version()).unwrap();
        assert_eq!(interner.vocabulary, vec!["hello", "world"]);
        let interner2 = interner.add_text("test hello");
        assert_eq!(interner2.vocabulary, vec!["hello", "world", "test"]);
    }
    #[test]
    fn test_add_builds_prefix_mapping() {
        let holder = InMemoryInternerHolder::with_seed("a b c", &mut crate::queue::MockQueue::new());
        let interner = holder.get(holder.latest_version()).unwrap();
        let vocab_len = interner.vocabulary.len();
        // prefix [0] should map to {1}
        let bitset_0 = interner.prefix_to_completions.get(&vec![0]).unwrap();
        let mut expected_0 = FixedBitSet::with_capacity(vocab_len);
        expected_0.grow(vocab_len);
        expected_0.insert(1);
        assert_eq!(*bitset_0, expected_0, "prefix [0] mismatch");
        // prefix [1] should map to {2}
        let bitset_1 = interner.prefix_to_completions.get(&vec![1]).unwrap();
        let mut expected_1 = FixedBitSet::with_capacity(vocab_len);
        expected_1.grow(vocab_len);
        expected_1.insert(2);
        assert_eq!(*bitset_1, expected_1, "prefix [1] mismatch");
        // prefix [0, 1] should map to {2}
        let bitset_01 = interner.prefix_to_completions.get(&vec![0, 1]).unwrap();
        let mut expected_01 = FixedBitSet::with_capacity(vocab_len);
        expected_01.grow(vocab_len);
        expected_01.insert(2);
        assert_eq!(*bitset_01, expected_01, "prefix [0, 1] mismatch");
    }
    #[test]
    fn test_add_handles_longer_phrases() {
        let holder = InMemoryInternerHolder::with_seed("a b c", &mut crate::queue::MockQueue::new());
        let interner = holder.get(holder.latest_version()).unwrap();
        // Should have prefix [0] -> bit 1 set (for "b")
        let prefix_0 = vec![0];
        let bitset_0 = interner.prefix_to_completions.get(&prefix_0).unwrap();
        assert!(!bitset_0.contains(0));
        assert!(bitset_0.contains(1));
        assert!(!bitset_0.contains(2));
        // Should have prefix [0, 1] -> bit 2 set (for "c")
        let prefix_01 = vec![0, 1];
        let bitset_01 = interner.prefix_to_completions.get(&prefix_01).unwrap();
        assert!(!bitset_01.contains(0));
        assert!(!bitset_01.contains(1));
        assert!(bitset_01.contains(2));
    }
    #[test]
    fn test_add_extends_existing_bitsets() {
        // First add with 2 words
        let holder = InMemoryInternerHolder::with_seed("a b", &mut crate::queue::MockQueue::new());
        let interner = holder.get(holder.latest_version()).unwrap();
        // Second add with 1 more word
        let interner2 = interner.add_text("a c");
        let prefix = vec![0];
        let bitset = interner2.prefix_to_completions.get(&prefix).unwrap();
        // Bitset should now have length 3 and both bits 1 and 2 should be set
        assert_eq!(bitset.len(), 3);
        assert!(!bitset.contains(0));
        assert!(bitset.contains(1)); // from first add
        assert!(bitset.contains(2)); // from second add
    }
    #[test]
    fn test_get_required_bits() {
        let holder = InMemoryInternerHolder::with_seed("a b c", &mut crate::queue::MockQueue::new());
        let interner = holder.get(holder.latest_version()).unwrap();
        // prefix [0] should map to {1}
        let required = vec![vec![0]];
        let bits = interner.get_required_bits(&required);
        // The bitset for prefix [0] should have bit 1 set
        let bitset = interner.prefix_to_completions.get(&vec![0]).unwrap();
        assert_eq!(bits, *bitset);
    }
    #[test]
    fn test_string_for_index() {
        let holder = InMemoryInternerHolder::with_seed("foo bar baz", &mut crate::queue::MockQueue::new());
        let interner = holder.get(holder.latest_version()).unwrap();
        let vocab = interner.vocabulary();
        assert_eq!(interner.string_for_index(0), vocab[0]);
        assert_eq!(interner.string_for_index(1), vocab[1]);
        assert_eq!(interner.string_for_index(2), vocab[2]);
    }
    #[test]
    #[should_panic(expected = "Index out of bounds in Interner::string_for_index")]
    fn test_string_for_index_out_of_bounds_panics() {
        let holder = InMemoryInternerHolder::with_seed("foo bar baz", &mut crate::queue::MockQueue::new());
        let interner = holder.get(holder.latest_version()).unwrap();
        interner.string_for_index(3);
    }
}

#[cfg(test)]
mod container_tests {
    use super::*;
    #[test]
    fn test_insert_and_get() {
        let holder = InMemoryInternerHolder::with_seed("a b", &mut crate::queue::MockQueue::new());
        let latest_version = holder.latest_version();
        let interner = holder.get(latest_version).unwrap();
        assert_eq!(interner.version(), latest_version);
    }
}

#[cfg(test)]
mod holder_tests {
    use super::*;
    use crate::queue::MockQueue;
    #[test]
    fn test_holder_new_initializes_empty() {
        let holder = InMemoryInternerHolder::with_seed("", &mut MockQueue::new());
        assert_eq!(holder.interners.len(), 1);
        assert_eq!(holder.latest_version(), 1);
    }
    #[test]
    fn test_holder_add_text_with_seed_increments_version() {
        let mut queue = MockQueue::new();
        let mut holder = InMemoryInternerHolder::with_seed("", &mut queue);
        holder.add_text_with_seed("foo bar", &mut queue);
        assert_eq!(holder.latest_version(), 2);
        holder.add_text_with_seed("baz", &mut queue);
        assert_eq!(holder.latest_version(), 3);
    }
    #[test]
    fn test_holder_latest_version_returns_correct_value() {
        let mut queue = MockQueue::new();
        let mut holder = InMemoryInternerHolder::with_seed("", &mut queue);
        holder.add_text_with_seed("a b", &mut queue);
        holder.add_text_with_seed("c", &mut queue);
        assert_eq!(holder.latest_version(), 3);
    }
}

#[cfg(test)]
mod intersect_logic_tests {
    use super::*;
    use fixedbitset::FixedBitSet;

    fn make_interner_with_vocab(vocab: Vec<&str>, prefix_map: Vec<(Vec<usize>, Vec<u32>)>) -> Interner {
        let vocabulary = vocab.into_iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let mut prefix_to_completions = std::collections::HashMap::new();
        let vocab_len = vocabulary.len();
        for (prefix, completions) in prefix_map {
            let mut fbs = FixedBitSet::with_capacity(vocab_len);
            fbs.grow(vocab_len);
            for idx in completions {
                fbs.insert(idx as usize);
            }
            prefix_to_completions.insert(prefix, fbs);
        }
        Interner {
            version: 1,
            vocabulary,
            prefix_to_completions,
        }
    }

    #[test]
    fn test_intersect_all_empty_returns_all_indexes() {
        let interner = make_interner_with_vocab(vec!["a", "b", "c"], vec![]);
        let result = interner.intersect(&[], &[]);
        assert_eq!(result, vec![0, 1, 2]);
    }

    #[test]
    fn test_intersect_required_and_forbidden() {
        // required: [00110, 00010] (as bitsets)
        // forbidden: [1]
        // expected: 00010 (index 3)
        let interner = make_interner_with_vocab(
            vec!["a", "b", "c", "d", "e"],
            vec![
                (vec![0], vec![2, 3]), // 00110
                (vec![1], vec![3]),    // 00010
            ],
        );
        let required = vec![vec![0], vec![1]];
        let forbidden = vec![1];
        let result = interner.intersect(&required, &forbidden);
        // Only index 3 should be present
        assert_eq!(result, vec![3]);
    }

    #[test]
    fn test_intersect_required_anded() {
        // required: [101, 110] => AND = 100
        let interner = make_interner_with_vocab(
            vec!["a", "b", "c"],
            vec![
                (vec![0], vec![0, 2]), // 101
                (vec![1], vec![0, 1]), // 110
            ],
        );
        let required = vec![vec![0], vec![1]];
        let forbidden = vec![];
        let result = interner.intersect(&required, &forbidden);
        // Only index 0 should be present
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn test_intersect_forbidden_zeroes_out() {
        // required: [111]
        // forbidden: [1]
        let interner = make_interner_with_vocab(
            vec!["a", "b", "c"],
            vec![(vec![0], vec![0, 1, 2])], // 111
        );
        let required = vec![vec![0]];
        let forbidden = vec![1];
        let result = interner.intersect(&required, &forbidden);
        // Should be [0, 2]
        assert_eq!(result, vec![0, 2]);
    }

    #[test]
    fn test_intersect_bug_case() {
        // This test is expected to fail with the current implementation
        // required: [00110, 00010] (as bitsets)
        // forbidden: []
        // expected: 00010 (index 3)
        let interner = make_interner_with_vocab(
            vec!["a", "b", "c", "d", "e"],
            vec![
                (vec![0], vec![2, 3]), // 00110
                (vec![1], vec![3]),    // 00010
            ],
        );
        let required = vec![vec![0], vec![1]];
        let forbidden = vec![];
        let result = interner.intersect(&required, &forbidden);
        // Only index 3 should be present
        assert_eq!(result, vec![3]); // This will fail with the current code
    }
}
