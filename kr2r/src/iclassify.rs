use crate::compact_hash::{CHTable, Compact};
use crate::seq::SeqReads;
use crate::taxonomy::Taxonomy;
use crate::Meros;
use crate::TaxonCounts;
// use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

pub const TAXID_MAX: u32 = u32::MAX - 1;
pub const MATE_PAIR_BORDER_TAXON: u32 = TAXID_MAX;
pub const READING_FRAME_BORDER_TAXON: u32 = TAXID_MAX - 1;
pub const AMBIGUOUS_SPAN_TAXON: u32 = TAXID_MAX - 2;

/// classify sequence
pub fn classify_sequence<'a, B: Compact>(
    taxonomy: &Taxonomy,
    cht: &CHTable<B>,
    seq_reads: SeqReads,
    meros: Meros,
    confidence_threshold: f64,
    minimum_hit_groups: i32,
) -> String {
    let mut hit_counts = TaxonCounts::new();
    let mut total_kmers = 0usize;
    let mut minimizer_hit_groups = 0;
    let mut last_minimizer = std::u64::MAX;

    let min_hash_check = meros.min_clear_hash_value.unwrap_or_default();

    for hash_keys in seq_reads.seq_paired {
        for &hashed in hash_keys.iter() {
            let taxon = if hashed >= min_hash_check {
                cht.get(hashed).to_u32()
            } else {
                0
            };

            if last_minimizer != hashed {
                if taxon > 0 {
                    minimizer_hit_groups += 1;
                }
                last_minimizer = hashed;
            }

            if taxon > 0 {
                *hit_counts.entry(taxon).or_insert(0) += 1;
            }
            total_kmers += 1;
        }
    }

    let mut call = resolve_tree(&hit_counts, taxonomy, total_kmers, confidence_threshold);
    if call > 0 && minimizer_hit_groups < minimum_hit_groups {
        call = 0;
    };

    let ext_call = taxonomy.nodes[call as usize].external_id;
    let classify = if call > 0 { "C" } else { "U" };
    format!(
        "{}\t{}\t{}",
        classify,
        trim_pair_info(&seq_reads.dna_id),
        ext_call
    )
}

pub fn trim_pair_info(id: &str) -> String {
    let sz = id.len();
    if sz <= 2 {
        return id.to_string();
    }
    if id.ends_with("/1") || id.ends_with("/2") {
        return id[0..sz - 2].to_string();
    }
    id.to_string()
}

pub fn count_values(vec: Vec<u32>, value_mask: usize) -> (HashMap<u32, u64>, usize) {
    let mut counts = HashMap::new();

    let mut unique_elements = HashSet::new();

    for value in vec {
        // 使用entry API处理计数
        // entry返回的是一个Entry枚举，它代表了可能存在也可能不存在的值
        // or_insert方法在键不存在时插入默认值（在这里是0）
        // 然后无论哪种情况，我们都对计数器加1
        let key = value.right(value_mask);
        *counts.entry(key).or_insert(0) += 1;
        unique_elements.insert(value);
    }

    (counts, unique_elements.len())
}

// &HashMap<u32, u64>,
pub fn resolve_tree(
    hit_counts: &HashMap<u32, u64>,
    taxonomy: &Taxonomy,
    total_minimizers: usize,
    confidence_threshold: f64,
) -> u32 {
    let required_score = (confidence_threshold * total_minimizers as f64).ceil() as u64;

    let mut max_taxon = 0u32;
    let mut max_score = 0;

    for (&taxon, _) in hit_counts {
        let mut score = 0;

        for (&taxon2, &count2) in hit_counts {
            if taxonomy.is_a_ancestor_of_b(taxon2, taxon) {
                score += count2;
            }
        }

        if score > max_score {
            max_score = score;
            max_taxon = taxon;
        } else if score == max_score {
            max_taxon = taxonomy.lca(max_taxon, taxon);
        }
    }

    max_score = *hit_counts.get(&max_taxon).unwrap_or(&0);

    while max_taxon != 0 && max_score < required_score {
        max_score = hit_counts
            .iter()
            .filter(|(&taxon, _)| taxonomy.is_a_ancestor_of_b(max_taxon, taxon))
            .map(|(_, &count)| count)
            .sum();

        if max_score >= required_score {
            break;
        }
        max_taxon = taxonomy.nodes[max_taxon as usize].parent_id as u32;
    }

    max_taxon
}

pub fn add_hitlist_string(taxa: &[u32], taxonomy: &Taxonomy) -> String {
    let mut result = String::new();
    let mut last_code = taxa[0];
    let mut code_count = 1;

    for &code in &taxa[1..] {
        if code == last_code {
            code_count += 1;
        } else {
            match last_code {
                MATE_PAIR_BORDER_TAXON => result.push_str("|:| "),
                READING_FRAME_BORDER_TAXON => result.push_str("-:- "),
                AMBIGUOUS_SPAN_TAXON => result.push_str(&format!("A:{} ", code_count)),
                _ => {
                    let ext_code = taxonomy.nodes[last_code as usize].external_id;
                    result.push_str(&format!("{}:{} ", ext_code, code_count));
                }
            }
            code_count = 1;
            last_code = code;
        }
    }
    match last_code {
        MATE_PAIR_BORDER_TAXON => result.push_str("|:|"),
        READING_FRAME_BORDER_TAXON => result.push_str("-:-"),
        AMBIGUOUS_SPAN_TAXON => result.push_str(&format!("A:{}", code_count)),
        _ => {
            let ext_code = taxonomy.nodes[last_code as usize].external_id;
            result.push_str(&format!("{}:{}", ext_code, code_count));
        }
    }

    result
}
