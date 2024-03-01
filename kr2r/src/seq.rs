use crate::mmscanner::KmerIterator;
use seq_io::fasta;
use seq_io::fasta::Record as FaRecord;
use seq_io::fastq;
use seq_io::fastq::Record as FqRecord;

use seq_io::parallel::Reader;

use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::iter;
use std::path::Path;

use seq_io::policy::StdPolicy;

use crate::Meros;

type DefaultBufPolicy = StdPolicy;

pub struct PairReader<R: io::Read, P = DefaultBufPolicy> {
    reader1: fastq::Reader<R, P>,
    reader2: fastq::Reader<R, P>,
}

impl Default for PairRecordSet {
    fn default() -> Self {
        PairRecordSet(fastq::RecordSet::default(), fastq::RecordSet::default())
    }
}

impl PairReader<File, DefaultBufPolicy> {
    /// Creates a reader from a file path.
    #[inline]
    pub fn from_path<P: AsRef<Path>>(path1: P, path2: P) -> io::Result<PairReader<File>> {
        // 分别打开两个文件
        let file1 = File::open(path1)?;
        let file2 = File::open(path2)?;

        // 为每个文件创建一个 fastq::Reader 实例
        let reader1 = fastq::Reader::new(file1);
        let reader2 = fastq::Reader::new(file2);

        // 使用这两个实例构造一个 PairReader 对象
        Ok(PairReader { reader1, reader2 })
    }
}

pub struct PairRecordSet(fastq::RecordSet, fastq::RecordSet);

impl<'a> iter::IntoIterator for &'a PairRecordSet {
    type Item = (fastq::RefRecord<'a>, fastq::RefRecord<'a>);
    type IntoIter = PairRecordSetIter<'a>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        PairRecordSetIter(self.0.into_iter(), self.1.into_iter())
    }
}

pub struct PairRecordSetIter<'a>(fastq::RecordSetIter<'a>, fastq::RecordSetIter<'a>);

impl<'a> Iterator for PairRecordSetIter<'a> {
    type Item = (fastq::RefRecord<'a>, fastq::RefRecord<'a>);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match (self.0.next(), self.1.next()) {
            (Some(record1), Some(record2)) => Some((record1, record2)),
            _ => None, // Return None if either iterator runs out of records
        }
    }
}

impl<R, P> Reader for PairReader<R, P>
where
    R: io::Read,
    P: seq_io::policy::BufPolicy + Send,
{
    type DataSet = PairRecordSet;
    type Err = fastq::Error;

    #[inline]
    fn fill_data(&mut self, rset: &mut PairRecordSet) -> Option<Result<(), Self::Err>> {
        let res1 = self.reader1.read_record_set(&mut rset.0)?.is_err();
        let res2 = self.reader2.read_record_set(&mut rset.1)?.is_err();

        if res1 || res2 {
            return None;
        }

        // If both reads are successful, return Ok(())
        Some(Ok(()))
    }
}

pub trait SeqX {
    fn seq_x(&self, score: i32) -> Vec<u8>;
}

impl<'a> SeqX for fastq::RefRecord<'a> {
    fn seq_x(&self, score: i32) -> Vec<u8> {
        if score <= 0 {
            return self.seq().to_vec();
        }

        let qual = self.qual();
        self.seq()
            .iter()
            .zip(qual.iter())
            .map(|(&base, &qscore)| {
                if (qscore as i32 - '!' as i32) < score {
                    b'x'
                } else {
                    base
                }
            })
            .collect::<Vec<u8>>()
    }
}

impl<'a> SeqX for fasta::RefRecord<'a> {
    #[allow(unused_variables)]
    fn seq_x(&self, score: i32) -> Vec<u8> {
        self.seq().to_vec()
    }
}

#[derive(Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct SeqReads {
    pub dna_id: String,
    pub seq_paired: Vec<Vec<u64>>,
}

pub trait SeqSet {
    fn to_seq_reads(&self, score: i32, meros: Meros) -> HashSet<SeqReads>;
}

impl SeqSet for PairRecordSet {
    fn to_seq_reads(&self, score: i32, meros: Meros) -> HashSet<SeqReads> {
        let mut seq_pair_set = HashSet::<SeqReads>::new();

        for records in self.into_iter() {
            let dna_id = records.0.id().unwrap_or_default().to_string();
            let seq1 = records.0.seq_x(score);
            let seq2 = records.1.seq_x(score);

            let kmers1 = KmerIterator::new(&seq1, meros).collect();
            let kmers2 = KmerIterator::new(&seq2, meros).collect();

            let seq_paired: Vec<Vec<u64>> = vec![kmers1, kmers2];
            seq_pair_set.insert(SeqReads { dna_id, seq_paired });
        }
        seq_pair_set
    }
}

impl SeqSet for fastq::RecordSet {
    fn to_seq_reads(&self, score: i32, meros: Meros) -> HashSet<SeqReads> {
        let mut seq_pair_set = HashSet::<SeqReads>::new();
        for records in self.into_iter() {
            let dna_id = records.id().unwrap_or_default().to_string();
            let seq1 = records.seq_x(score);
            let kmers1: Vec<u64> = KmerIterator::new(&seq1, meros).collect();
            let seq_paired: Vec<Vec<u64>> = vec![kmers1];
            seq_pair_set.insert(SeqReads { dna_id, seq_paired });
        }
        seq_pair_set
    }
}