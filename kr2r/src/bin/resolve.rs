use clap::Parser;
use dashmap::DashMap;
use kr2r::compact_hash::{Compact, HashConfig};
use kr2r::iclassify::{count_values, resolve_tree};
use kr2r::taxonomy::Taxonomy;
use kr2r::utils::find_and_sort_files;
use rayon::prelude::*;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Result, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const BATCH_SIZE: usize = 8 * 1024 * 1024;

pub fn read_id_to_seq_map<P: AsRef<Path>>(filename: P) -> Result<DashMap<u32, (String, usize)>> {
    let file = File::open(filename)?;
    let reader = BufReader::new(file);
    let id_map = DashMap::new();

    reader.lines().par_bridge().for_each(|line| {
        let line = line.expect("Could not read line");
        let parts: Vec<&str> = line.trim().split_whitespace().collect();
        if parts.len() >= 3 {
            // 解析序号为u32类型的键
            if let Ok(id) = parts[0].parse::<u32>() {
                // 第二列是序列标识符，直接作为字符串
                let seq_id = parts[1].to_string();
                if let Ok(count) = parts[2].parse::<usize>() {
                    // 插入到DashMap中
                    id_map.insert(id, (seq_id, count));
                }
            }
        }
    });

    Ok(id_map)
}

#[derive(Parser, Debug, Clone)]
#[clap(
    version,
    about = "resolve taxonomy tree",
    long_about = "resolve taxonomy tree"
)]
pub struct Args {
    /// database hash chunk directory and other files
    #[clap(long)]
    hash_dir: PathBuf,

    // chunk directory
    #[clap(long, value_parser, required = true)]
    chunk_dir: PathBuf,

    // /// The file path for the Kraken 2 index.
    // #[clap(short = 'H', long = "index-filename", value_parser, required = true)]
    // index_filename: PathBuf,

    // /// The file path for the Kraken 2 taxonomy.
    // #[clap(short = 't', long = "taxonomy-filename", value_parser, required = true)]
    // taxonomy_filename: String,
    /// Confidence score threshold, default is 0.0.
    #[clap(
        short = 'T',
        long = "confidence-threshold",
        value_parser,
        default_value_t = 0.0
    )]
    confidence_threshold: f64,

    /// The minimum number of hit groups needed for a call.
    #[clap(
        short = 'g',
        long = "minimum-hit-groups",
        value_parser,
        default_value_t = 2
    )]
    minimum_hit_groups: usize,

    /// 批量处理大小 default: 8MB
    #[clap(long, default_value_t = BATCH_SIZE)]
    batch_size: usize,

    /// File path for outputting normal Kraken output.
    #[clap(long = "output-dir", value_parser)]
    kraken_output_dir: Option<PathBuf>,
}

fn process_batch<P: AsRef<Path>, B: Compact>(
    sample_file: P,
    args: &Args,
    taxonomy: &Taxonomy,
    id_map: DashMap<u32, (String, usize)>,
    writer: Box<dyn Write + Send>,
    value_mask: usize,
) -> Result<()> {
    let file = File::open(sample_file)?;
    let mut reader = BufReader::new(file);
    let size = std::mem::size_of::<B>();
    let mut batch_buffer = vec![0u8; size * BATCH_SIZE];

    let hit_counts = DashMap::new();
    let confidence_threshold = args.confidence_threshold;
    let minimum_hit_groups = args.minimum_hit_groups;

    while let Ok(bytes_read) = reader.read(&mut batch_buffer) {
        if bytes_read == 0 {
            break;
        } // 文件末尾

        // 处理读取的数据批次
        let slots_in_batch = bytes_read / size;

        let slots = unsafe {
            std::slice::from_raw_parts(batch_buffer.as_ptr() as *const B, slots_in_batch)
        };

        slots.into_par_iter().for_each(|item| {
            let taxid = item.left(0).to_u32();
            let seq_id = item.right(0).to_u32();
            hit_counts
                .entry(seq_id)
                .or_insert_with(Vec::new)
                .push(taxid)
        });
    }

    let writer = Mutex::new(writer);

    hit_counts.into_par_iter().for_each(|(k, v)| {
        if let Some(item) = id_map.get(&k) {
            let total_kmers: usize = item.1;
            // let minimizer_hit_groups = v.len();
            let (counts, minimizer_hit_groups) = count_values(v, value_mask);
            let mut call = resolve_tree(&counts, taxonomy, total_kmers, confidence_threshold);
            if call > 0 && minimizer_hit_groups < minimum_hit_groups {
                call = 0;
            };

            let ext_call = taxonomy.nodes[call as usize].external_id;
            let classify = if call > 0 { "C" } else { "U" };
            let output_line = format!("{}\t{}\t{}\n", classify, item.0, ext_call);
            // 使用锁来同步写入
            let mut file = writer.lock().unwrap();
            file.write_all(output_line.as_bytes()).unwrap();
        }
    });
    Ok(())
}

pub fn run(args: Args) -> Result<()> {
    let hash_dir = &args.hash_dir;
    let taxonomy_filename = hash_dir.join("taxo.k2d");
    let taxo = Taxonomy::from_file(taxonomy_filename)?;

    let sample_files = find_and_sort_files(&args.chunk_dir, "sample_file", ".bin")?;
    let sample_id_files = find_and_sort_files(&args.chunk_dir, "sample_id", ".map")?;

    let partition = sample_files.len();
    let hash_config = HashConfig::<u32>::from_hash_header(&args.hash_dir.join("hash_config.k2d"))?;
    let value_mask = hash_config.value_mask;
    for i in 0..partition {
        let sample_file = &sample_files[i];
        let sample_id_map = read_id_to_seq_map(&sample_id_files[i])?;
        let writer: Box<dyn Write + Send> = match &args.kraken_output_dir {
            Some(ref file_path) => {
                let filename = file_path.join(format!("output_{}.txt", i + 1));
                let file = File::create(filename)?;
                Box::new(BufWriter::new(file)) as Box<dyn Write + Send>
            }
            None => Box::new(io::stdout()) as Box<dyn Write + Send>,
        };
        process_batch::<&PathBuf, u64>(
            sample_file,
            &args,
            &taxo,
            sample_id_map,
            writer,
            value_mask,
        )?;
    }
    Ok(())
}

#[allow(dead_code)]
fn main() {
    let args = Args::parse();
    if let Err(e) = run(args) {
        eprintln!("Application error: {}", e);
    }
}
