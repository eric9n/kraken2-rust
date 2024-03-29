use clap::Parser;
use kr2r::compact_hash::CHTable;
use kr2r::iclassify::classify_sequence;
use kr2r::seq::{self, SeqSet};
use kr2r::taxonomy::Taxonomy;
use kr2r::utils::{detect_file_format, FileFormat};
use kr2r::IndexOptions;
use rayon::prelude::*;
use seq_io::fasta::Reader as FaReader;
use seq_io::fastq::Reader as FqReader;
use seq_io::parallel::read_parallel;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::io::{Error, ErrorKind, Result};
use std::time::Instant;

/// Command line arguments for the classify program.
///
/// This structure defines the command line arguments that are accepted by the classify program.
/// It uses the `clap` crate for parsing command line arguments.
#[derive(Parser, Debug, Clone)]
#[clap(
    version,
    about = "classify",
    long_about = "classify a set of sequences"
)]
struct Args {
    /// The file path for the Kraken 2 index.
    #[clap(short = 'H', long = "index-filename", value_parser, required = true)]
    index_filename: String,

    /// The file path for the Kraken 2 taxonomy.
    #[clap(short = 't', long = "taxonomy-filename", value_parser, required = true)]
    taxonomy_filename: String,

    /// The file path for the Kraken 2 options.
    #[clap(short = 'o', long = "options-filename", value_parser, required = true)]
    options_filename: String,

    /// Confidence score threshold, default is 0.0.
    #[clap(
        short = 'T',
        long = "confidence-threshold",
        value_parser,
        default_value_t = 0.0
    )]
    confidence_threshold: f64,

    // /// Enable quick mode for faster processing.
    // #[clap(short = 'q', long = "quick-mode", action)]
    // quick_mode: bool,
    /// The number of threads to use, default is 1.
    #[clap(short = 'p', long = "num-threads", value_parser, default_value_t = 1)]
    num_threads: i32,

    /// The minimum number of hit groups needed for a call.
    #[clap(
        short = 'g',
        long = "minimum-hit-groups",
        value_parser,
        default_value_t = 2
    )]
    minimum_hit_groups: i32,

    /// Enable paired-end processing.
    #[clap(short = 'P', long = "paired-end-processing", action)]
    paired_end_processing: bool,

    /// Process pairs with mates in the same file.
    #[clap(short = 'S', long = "single-file-pairs", action)]
    single_file_pairs: bool,

    // /// Use mpa-style report format.
    // #[clap(short = 'm', long = "mpa-style-report", action)]
    // mpa_style_report: bool,

    // /// Report k-mer data in the output.
    // #[clap(short = 'K', long = "report-kmer-data", action)]
    // report_kmer_data: bool,

    // /// File path for outputting the report.
    // #[clap(short = 'R', long = "report-filename", value_parser)]
    // report_filename: Option<String>,

    // /// Report taxa with zero count.
    // #[clap(short = 'z', long = "report-zero-counts", action)]
    // report_zero_counts: bool,

    // /// File path for outputting classified sequences.
    // #[clap(short = 'C', long = "classified-output-filename", value_parser)]
    // classified_output_filename: Option<String>,

    // /// File path for outputting unclassified sequences.
    // #[clap(short = 'U', long = "unclassified-output-filename", value_parser)]
    // unclassified_output_filename: Option<String>,
    /// File path for outputting normal Kraken output.
    #[clap(short = 'O', long = "kraken-output-filename", value_parser)]
    kraken_output_filename: Option<String>,

    // /// Print scientific name instead of taxid in Kraken output.
    // #[clap(short = 'n', long = "print-scientific-name", action)]
    // print_scientific_name: bool,
    /// Minimum quality score for FASTQ data, default is 0.
    #[clap(
        short = 'Q',
        long = "minimum-quality-score",
        value_parser,
        default_value_t = 0
    )]
    minimum_quality_score: i32,

    /// Input files for processing.
    ///
    /// A list of input file paths (FASTA/FASTQ) to be processed by the classify program.
    // #[clap(short = 'F', long = "files")]
    input_files: Vec<String>,
}

fn check_feature(dna_db: bool) -> Result<()> {
    #[cfg(feature = "dna")]
    if !dna_db {
        return Err(Error::new(
            ErrorKind::Other,
            "Feature 'dna' is enabled but 'dna_db' is false",
        ));
    }

    #[cfg(feature = "protein")]
    if dna_db {
        return Err(Error::new(
            ErrorKind::Other,
            "Feature 'protein' is enabled but 'dna_db' is true",
        ));
    }

    Ok(())
}

macro_rules! process_file_pairs {
    ($taxonomy:expr, $cht:expr, $args:expr, $meros:expr, $reader_creator:expr) => {
        // 对 file1 和 file2 执行分类处理
        {
            let mut writer: Box<dyn Write> = match $args.kraken_output_filename {
                Some(ref filename) => {
                    let file = File::create(filename)?;
                    Box::new(BufWriter::new(file)) as Box<dyn Write>
                }
                None => Box::new(io::stdout()) as Box<dyn Write>,
            };
            let pair_reader = $reader_creator.expect("Unable to create pair reader from paths");
            read_parallel(
                pair_reader,
                $args.num_threads as u32,
                $args.num_threads as usize,
                |record_set| record_set.to_seq_reads($args.minimum_quality_score, $meros),
                |record_sets| {
                    while let Some(Ok((_, seq_pair_set))) = record_sets.next() {
                        let results: Vec<String> = seq_pair_set
                            .into_par_iter()
                            .map(|item| {
                                classify_sequence(
                                    &$taxonomy,
                                    &$cht,
                                    item,
                                    $meros,
                                    $args.confidence_threshold,
                                    $args.minimum_hit_groups,
                                )
                            })
                            .collect();
                        for result in results {
                            writeln!(writer, "{}", result).expect("Unable to write to file");
                        }
                    }
                },
            );
        }
    };
}

/// 处理fastq文件
fn process_files(
    args: Args,
    idx_opts: IndexOptions,
    cht: &CHTable<u32>,
    taxonomy: &Taxonomy,
) -> Result<()> {
    let meros = idx_opts.as_meros();

    if args.paired_end_processing && !args.single_file_pairs {
        // 处理成对的文件
        for file_pair in args.input_files.chunks(2) {
            let file1 = &file_pair[0];
            let file2 = &file_pair[1];
            match detect_file_format(&file1)? {
                FileFormat::Fastq => {
                    process_file_pairs!(
                        taxonomy,
                        cht,
                        args,
                        meros,
                        seq::PairFastqReader::from_path(file1, Some(file2))
                    );
                }
                FileFormat::Fasta => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "Unrecognized file format(fasta paired)",
                    ));
                }
            }
        }
    } else {
        for file in args.input_files {
            // 对 file 执行分类处理
            match detect_file_format(&file)? {
                FileFormat::Fastq => {
                    process_file_pairs!(taxonomy, cht, args, meros, FqReader::from_path(file));
                }
                FileFormat::Fasta => {
                    process_file_pairs!(taxonomy, cht, args, meros, FaReader::from_path(file));
                }
            };
        }
    }

    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    let idx_opts = IndexOptions::read_index_options(args.options_filename.clone())?;
    check_feature(idx_opts.dna_db)?;
    let taxo = Taxonomy::from_file(&args.taxonomy_filename)?;
    // let hash_config = HashConfig::<u32>::from(&args.index_filename)?;
    let cht = CHTable::from(args.index_filename.clone(), 0, 1)?;

    if args.paired_end_processing && !args.single_file_pairs && args.input_files.len() % 2 != 0 {
        // 验证文件列表是否为偶数个
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Paired-end processing requires an even number of input files.",
        ));
    }

    // 开始计时
    let start = Instant::now();

    process_files(args, idx_opts, &cht, &taxo)?;
    // 计算持续时间
    let duration = start.elapsed();

    // 打印运行时间
    println!("classify took: {:?}", duration);
    Ok(())
}
