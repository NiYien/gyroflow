#[cfg(not(feature = "neuflow"))]
fn main() {
    eprintln!("neuflow_bpk_tool requires `--features neuflow`");
    std::process::exit(1);
}

#[cfg(feature = "neuflow")]
mod app {
    use std::collections::BTreeMap;
    use std::env;
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::{Path, PathBuf};

    use burn::tensor::{DType, TensorData};
    use burn_store::{BurnpackStore, ModuleStore};
    use byteorder::{ByteOrder, LittleEndian};
    use serde::{Deserialize, Serialize};
    use sha2::{Digest, Sha256};

    const MAGIC_NUMBER: u32 = 0x4255524E;
    const FORMAT_VERSION: u16 = 0x0001;
    const HEADER_SIZE: usize = 10;
    const TENSOR_ALIGNMENT: u64 = 256;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct BurnpackMetadata {
        tensors: BTreeMap<String, TensorDescriptor>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        metadata: BTreeMap<String, String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TensorDescriptor {
        dtype: DType,
        shape: Vec<u64>,
        data_offsets: (u64, u64),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        param_id: Option<u64>,
    }

    #[derive(Debug, Clone)]
    struct TensorEntry {
        name: String,
        dtype: DType,
        shape: Vec<usize>,
        bytes: Vec<u8>,
        param_id: Option<u64>,
    }

    #[derive(Debug, Default, Clone, Copy)]
    struct ByteStats {
        tensors: usize,
        elements: usize,
        bytes: usize,
    }

    fn align_offset(offset: u64, alignment: u64) -> u64 {
        offset.div_ceil(alignment) * alignment
    }

    fn aligned_data_section_start(metadata_size: usize) -> usize {
        let unaligned = (HEADER_SIZE + metadata_size) as u64;
        (unaligned.div_ceil(TENSOR_ALIGNMENT) * TENSOR_ALIGNMENT) as usize
    }

    fn read_burnpack_metadata(input: &Path) -> Result<BurnpackMetadata, String> {
        let bytes = fs::read(input)
            .map_err(|e| format!("Failed to read {}: {e}", input.display()))?;
        if bytes.len() < HEADER_SIZE {
            return Err(format!("{} is too small to be a Burnpack file", input.display()));
        }
        let metadata_size = LittleEndian::read_u32(&bytes[6..10]) as usize;
        let metadata_start = HEADER_SIZE;
        let metadata_end = metadata_start + metadata_size;
        if metadata_end > bytes.len() {
            return Err(format!("{} has truncated metadata", input.display()));
        }
        let mut cursor = std::io::Cursor::new(&bytes[metadata_start..metadata_end]);
        ciborium::de::from_reader(&mut cursor)
            .map_err(|e| format!("Failed to decode burnpack metadata: {e}"))
    }

    fn hash_bytes(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    fn default_input_path() -> PathBuf {
        PathBuf::from("resources/neuflow_v2_clean.bpk")
    }

    fn default_output_path(input: &Path) -> PathBuf {
        let parent = input.parent().unwrap_or_else(|| Path::new("."));
        let stem = input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("neuflow_v2_clean");
        parent.join(format!("{stem}_optimized.bpk"))
    }

    fn dtype_name(dtype: DType) -> &'static str {
        match dtype {
            DType::F64 => "F64",
            DType::F32 | DType::Flex32 => "F32",
            DType::F16 => "F16",
            DType::BF16 => "BF16",
            DType::I64 => "I64",
            DType::I32 => "I32",
            DType::I16 => "I16",
            DType::I8 => "I8",
            DType::U64 => "U64",
            DType::U32 => "U32",
            DType::U16 => "U16",
            DType::U8 => "U8",
            DType::Bool(_) => "Bool",
            DType::QFloat(_) => "QFloat",
        }
    }

    fn read_entries(input: &Path) -> Result<Vec<TensorEntry>, String> {
        let path = input
            .to_str()
            .ok_or_else(|| format!("Unsupported path: {}", input.display()))?;
        let mut store = BurnpackStore::from_file(path);
        let snapshots = store
            .get_all_snapshots()
            .map_err(|e| format!("Failed to read burnpack snapshots: {e}"))?;

        let mut entries = Vec::with_capacity(snapshots.len());
        for (name, snapshot) in snapshots {
            let data = snapshot
                .to_data()
                .map_err(|e| format!("Failed to materialize tensor {name}: {e:?}"))?;
            entries.push(TensorEntry {
                name: name.clone(),
                dtype: snapshot.dtype,
                shape: snapshot.shape.iter().copied().collect(),
                bytes: data.bytes.to_vec(),
                param_id: snapshot.tensor_id.map(|id| id.val()),
            });
        }

        Ok(entries)
    }

    fn element_count(shape: &[usize]) -> usize {
        shape.iter().product()
    }

    fn analyze_entries(entries: &[TensorEntry], metadata: &BurnpackMetadata, file_size: u64) {
        let mut by_dtype: BTreeMap<&'static str, ByteStats> = BTreeMap::new();
        let mut constant_stats = ByteStats::default();
        let mut duplicate_groups: Vec<(usize, usize, String, Vec<usize>, Vec<String>)> = Vec::new();
        let mut groups: BTreeMap<(String, Vec<usize>, String), Vec<&TensorEntry>> = BTreeMap::new();
        let mut unique_ranges: BTreeMap<(u64, u64), usize> = BTreeMap::new();

        for entry in entries {
            let dtype = dtype_name(entry.dtype);
            let elems = element_count(&entry.shape);
            let stats = by_dtype.entry(dtype).or_default();
            stats.tensors += 1;
            stats.elements += elems;
            stats.bytes += entry.bytes.len();

            if entry.name.contains("constant") {
                constant_stats.tensors += 1;
                constant_stats.elements += elems;
                constant_stats.bytes += entry.bytes.len();
            }

            groups
                .entry((dtype.to_string(), entry.shape.clone(), hash_bytes(&entry.bytes)))
                .or_default()
                .push(entry);
        }

        for descriptor in metadata.tensors.values() {
            let len = (descriptor.data_offsets.1 - descriptor.data_offsets.0) as usize;
            unique_ranges.entry(descriptor.data_offsets).or_insert(len);
        }

        let mut duplicate_wasted_bytes = 0usize;
        for ((dtype, shape, _), members) in groups {
            if members.len() <= 1 {
                continue;
            }
            let wasted = (members.len() - 1) * members[0].bytes.len();
            duplicate_wasted_bytes += wasted;
            duplicate_groups.push((
                members.len(),
                wasted,
                dtype,
                shape,
                members.into_iter().map(|entry| entry.name.clone()).collect(),
            ));
        }

        duplicate_groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));

        println!("=== Burnpack Analysis ===");
        println!("file_size_bytes={file_size}");
        println!("file_size_mib={:.2}", file_size as f64 / 1024.0 / 1024.0);
        println!("tensor_count={}", entries.len());
        println!();
        println!("dtypes:");
        for (dtype, stats) in &by_dtype {
            println!(
                "  {dtype:<5} tensors={} elements={} bytes={}",
                stats.tensors, stats.elements, stats.bytes
            );
        }
        println!();
        println!(
            "constants: tensors={} elements={} bytes={}",
            constant_stats.tensors, constant_stats.elements, constant_stats.bytes
        );
        println!(
            "duplicate_groups={} duplicate_wasted_bytes={} duplicate_wasted_mib={:.2}",
            duplicate_groups.len(),
            duplicate_wasted_bytes,
            duplicate_wasted_bytes as f64 / 1024.0 / 1024.0
        );
        let physical_blob_bytes: usize = unique_ranges.values().sum();
        println!(
            "physical_unique_blobs={} physical_blob_bytes={} physical_blob_mib={:.2}",
            unique_ranges.len(),
            physical_blob_bytes,
            physical_blob_bytes as f64 / 1024.0 / 1024.0
        );
        println!();
        for (count, wasted, dtype, shape, names) in duplicate_groups.iter().take(12) {
            println!(
                "duplicate count={} wasted_bytes={} dtype={} shape={:?}",
                count, wasted, dtype, shape
            );
            for name in names.iter().take(8) {
                println!("  {name}");
            }
        }
    }

    fn maybe_convert_half(entry: &mut TensorEntry, convert_half: bool, keep_constants_f32: bool) {
        if !convert_half {
            return;
        }
        if !matches!(entry.dtype, DType::F32 | DType::Flex32) {
            return;
        }
        // When --keep-constants-f32, skip conversion for constant tensors and layernorm params
        if keep_constants_f32 {
            let name_lower = entry.name.to_lowercase();
            if name_lower.contains("constant") || name_lower.contains("layernorm") {
                return;
            }
        }

        let data = TensorData::from_bytes_vec(
            std::mem::take(&mut entry.bytes),
            entry.shape.clone(),
            entry.dtype,
        )
        .convert_dtype(DType::F16);
        entry.dtype = DType::F16;
        entry.bytes = data.bytes.to_vec();
    }

    /// Promote F16 constant/layernorm tensors to F32 (upcast).
    /// This allows the GPU to use them in F32 computation paths.
    fn maybe_promote_constants(entry: &mut TensorEntry) {
        if entry.dtype != DType::F16 {
            return;
        }
        let name_lower = entry.name.to_lowercase();
        if !name_lower.contains("constant") && !name_lower.contains("layernorm") {
            return;
        }
        let data = TensorData::from_bytes_vec(
            std::mem::take(&mut entry.bytes),
            entry.shape.clone(),
            entry.dtype,
        )
        .convert_dtype(DType::F32);
        entry.dtype = DType::F32;
        entry.bytes = data.bytes.to_vec();
    }

    fn write_optimized_burnpack(
        entries: &[TensorEntry],
        output: &Path,
        metadata: BTreeMap<String, String>,
        dedupe: bool,
    ) -> Result<(), String> {
        let mut descriptors = BTreeMap::new();
        let mut unique_blobs: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut shared_offsets: BTreeMap<(String, Vec<usize>, String), (u64, u64)> = BTreeMap::new();
        let mut current_offset = 0u64;

        for entry in entries {
            let signature = if dedupe {
                (
                    dtype_name(entry.dtype).to_string(),
                    entry.shape.clone(),
                    hash_bytes(&entry.bytes),
                )
            } else {
                (
                    format!("{}:{}", dtype_name(entry.dtype), entry.name),
                    entry.shape.clone(),
                    hash_bytes(&entry.bytes),
                )
            };

            let (start, end) = if let Some((start, end)) = shared_offsets.get(&signature) {
                (*start, *end)
            } else {
                let start = align_offset(current_offset, TENSOR_ALIGNMENT);
                let end = start + entry.bytes.len() as u64;
                shared_offsets.insert(signature, (start, end));
                unique_blobs.push((start, entry.bytes.clone()));
                current_offset = end;
                (start, end)
            };

            descriptors.insert(
                entry.name.clone(),
                TensorDescriptor {
                    dtype: entry.dtype,
                    shape: entry.shape.iter().map(|&dim| dim as u64).collect(),
                    data_offsets: (start, end),
                    param_id: entry.param_id,
                },
            );
        }

        let burnpack_metadata = BurnpackMetadata {
            tensors: descriptors,
            metadata,
        };
        let mut metadata_bytes = Vec::new();
        ciborium::ser::into_writer(&burnpack_metadata, &mut metadata_bytes)
            .map_err(|e| format!("Failed to encode metadata: {e}"))?;

        let metadata_size = metadata_bytes.len() as u32;
        let data_section_start = aligned_data_section_start(metadata_bytes.len());
        let data_size = unique_blobs
            .iter()
            .map(|(offset, bytes)| (*offset as usize) + bytes.len())
            .max()
            .unwrap_or(0);
        let total_size = data_section_start + data_size;

        let mut buffer = vec![0u8; total_size];
        LittleEndian::write_u32(&mut buffer[0..4], MAGIC_NUMBER);
        LittleEndian::write_u16(&mut buffer[4..6], FORMAT_VERSION);
        LittleEndian::write_u32(&mut buffer[6..10], metadata_size);
        buffer[HEADER_SIZE..HEADER_SIZE + metadata_bytes.len()].copy_from_slice(&metadata_bytes);

        for (offset, bytes) in unique_blobs {
            let start = data_section_start + offset as usize;
            let end = start + bytes.len();
            buffer[start..end].copy_from_slice(&bytes);
        }

        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
        }
        let mut file = File::create(output)
            .map_err(|e| format!("Failed to create {}: {e}", output.display()))?;
        file.write_all(&buffer)
            .map_err(|e| format!("Failed to write {}: {e}", output.display()))?;
        file.flush()
            .map_err(|e| format!("Failed to flush {}: {e}", output.display()))?;

        Ok(())
    }

    fn optimize(input: &Path, output: &Path, convert_half: bool, dedupe: bool, keep_constants_f32: bool) -> Result<(), String> {
        let entries = read_entries(input)?;
        let mut optimized = entries.clone();
        let mut converted = 0usize;
        let mut kept_f32 = 0usize;
        let mut promoted = 0usize;
        for entry in &mut optimized {
            if keep_constants_f32 {
                let before = entry.dtype;
                maybe_promote_constants(entry);
                if before != entry.dtype {
                    promoted += 1;
                    continue; // already promoted, skip half conversion
                }
            }
            let before = entry.dtype;
            maybe_convert_half(entry, convert_half, keep_constants_f32);
            if before != entry.dtype {
                converted += 1;
            } else if keep_constants_f32 && matches!(before, DType::F32 | DType::Flex32) {
                kept_f32 += 1;
            }
        }

        let input_size = fs::metadata(input)
            .map_err(|e| format!("Failed to stat {}: {e}", input.display()))?
            .len();

        // We currently always write through the custom writer. When dedupe=false,
        // each tensor gets a unique signature because the hash key is unique per
        // tensor name.
        let metadata = BTreeMap::from([
            ("optimized_from".to_string(), input.display().to_string()),
            ("optimized_half".to_string(), convert_half.to_string()),
            ("optimized_dedupe".to_string(), dedupe.to_string()),
        ]);

        write_optimized_burnpack(&optimized, output, metadata, dedupe)?;

        let output_size = fs::metadata(output)
            .map_err(|e| format!("Failed to stat {}: {e}", output.display()))?
            .len();

        println!("=== Burnpack Optimize ===");
        println!("input={}", input.display());
        println!("output={}", output.display());
        println!("converted_f32_to_f16={converted}");
        println!("promoted_f16_to_f32={promoted}");
        println!("kept_f32_constants={kept_f32}");
        println!("dedupe_enabled={dedupe}");
        println!("input_size_bytes={input_size}");
        println!("output_size_bytes={output_size}");
        println!(
            "size_reduction_bytes={}",
            input_size.saturating_sub(output_size)
        );
        println!(
            "size_reduction_pct={:.2}",
            (1.0 - (output_size as f64 / input_size as f64)) * 100.0
        );

        let optimized_entries = read_entries(output)?;
        let optimized_metadata = read_burnpack_metadata(output)?;
        analyze_entries(&optimized_entries, &optimized_metadata, output_size);
        Ok(())
    }

    fn print_usage() {
        eprintln!("Usage:");
        eprintln!("  cargo run --manifest-path src/core/Cargo.toml --features neuflow --bin neuflow_bpk_tool -- analyze [input]");
        eprintln!("  cargo run --manifest-path src/core/Cargo.toml --features neuflow --bin neuflow_bpk_tool -- optimize [input] [output] [--no-half] [--no-dedupe] [--keep-constants-f32]");
    }

    pub fn main() {
        let args: Vec<String> = env::args().collect();
        if args.len() < 2 {
            print_usage();
            std::process::exit(1);
        }

        match args[1].as_str() {
            "analyze" => {
                let input = args.get(2).map(PathBuf::from).unwrap_or_else(default_input_path);
                match read_entries(&input)
                    .and_then(|entries| {
                        let size = fs::metadata(&input)
                            .map_err(|e| format!("Failed to stat {}: {e}", input.display()))?
                            .len();
                        let metadata = read_burnpack_metadata(&input)?;
                        analyze_entries(&entries, &metadata, size);
                        Ok(())
                    }) {
                    Ok(()) => {}
                    Err(err) => {
                        eprintln!("{err}");
                        std::process::exit(1);
                    }
                }
            }
            "optimize" => {
                let input = args.get(2).map(PathBuf::from).unwrap_or_else(default_input_path);
                let output = args
                    .get(3)
                    .map(PathBuf::from)
                    .unwrap_or_else(|| default_output_path(&input));
                let convert_half = !args.iter().any(|arg| arg == "--no-half");
                let dedupe = !args.iter().any(|arg| arg == "--no-dedupe");
                let keep_constants_f32 = args.iter().any(|arg| arg == "--keep-constants-f32");

                if let Err(err) = optimize(&input, &output, convert_half, dedupe, keep_constants_f32) {
                    eprintln!("{err}");
                    std::process::exit(1);
                }
            }
            _ => {
                print_usage();
                std::process::exit(1);
            }
        }
    }
}

#[cfg(feature = "neuflow")]
fn main() {
    app::main();
}
