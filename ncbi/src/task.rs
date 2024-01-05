use crate::load::NcbiFile;
use anyhow::Result;
use futures::stream::StreamExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};

async fn process_tasks(
    task_type: String,
    num_threads: usize,
    mut receiver: mpsc::Receiver<NcbiFile>,
    next_tx: Option<mpsc::Sender<NcbiFile>>,
) -> Result<usize> {
    let semaphore = Arc::new(Semaphore::new(num_threads));
    let mut futures = vec![];
    let counter: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));

    while let Some(task) = receiver.recv().await {
        let permit = semaphore.clone().acquire_owned().await?;
        let next_tx_clone = next_tx.clone();
        let task_type_clone = task_type.clone();
        let counter_clone = counter.clone();

        let task_future = tokio::spawn(async move {
            let result = match task_type_clone.as_str() {
                "run" => task.run().await,
                "check" => task.check().await,
                _ => unreachable!(),
            };
            drop(permit);
            if result.is_ok() {
                counter_clone.fetch_add(1, Ordering::SeqCst);
                if let Some(tx) = next_tx_clone {
                    let _ = tx.send(task).await;
                }
            }
            result
        });
        futures.push(task_future);
    }

    let mut stream = futures::stream::iter(futures).buffer_unordered(num_threads);

    while let Some(result) = stream.next().await {
        match result {
            Ok(Ok(_)) => {} // 任务成功完成
            Ok(Err(e)) => log::error!("Task failed: {}", e),
            Err(e) => log::error!("Task panicked or could not be joined: {}", e),
        }
    }

    Ok(counter.load(Ordering::SeqCst))
}

/// 处理 assembly 文件
async fn process_assembly_tasks(
    group: &str,
    data_dir: &PathBuf,
    tx: mpsc::Sender<NcbiFile>, // 发送端用于发送 `parse_assembly_file` 的结果
) -> Result<usize> {
    let counter = Arc::new(AtomicUsize::new(0));
    let down_tasks = NcbiFile::from_group(group, data_dir).await;
    let mut futures = vec![];
    for asbly in down_tasks {
        let data_dir_clone = data_dir.clone();
        let tx_clone = tx.clone();
        let counter_clone = counter.clone(); // 克隆 Arc
        match asbly.run().await {
            Ok(_) => {
                let task_future = tokio::spawn(async move {
                    if let Err(e) = asbly
                        .parse_assembly_file(&data_dir_clone, tx_clone, counter_clone)
                        .await
                    {
                        log::error!("Error parsing assembly file: {}", e);
                    }
                });
                futures.push(task_future);
            }
            Err(e) => {
                log::info!("{}", e);
            }
        }
    }
    let mut stream = futures::stream::iter(futures).buffer_unordered(2);

    while let Some(result) = stream.next().await {
        let _ = result;
    }

    Ok(counter.load(Ordering::SeqCst))
}

pub async fn run_task(group: &str, data_dir: &PathBuf, num_threads: usize) -> Result<()> {
    log::info!("{} download assembly file start...", group);
    let (tx, rx) = mpsc::channel(4096); // 通道大小可以根据需要调整
    let (tx1, rx1) = mpsc::channel(4096); // 通道大小可以根据需要调整
    let assembly_tasks = process_assembly_tasks(group, data_dir, tx);
    let download_handle = process_tasks("run".to_string(), num_threads, rx, Some(tx1));
    let md5_handle = process_tasks("check".to_string(), num_threads, rx1, None);
    // // 等待处理任务完成
    let (ably_res, down_res, md5_res) = tokio::join!(assembly_tasks, download_handle, md5_handle);
    log::info!(
        "{} file total count: {}, downloaded: {}, md5match: {}",
        group,
        ably_res?,
        down_res?,
        md5_res?
    );
    log::info!("{} file finished...", group);
    Ok(())
}

pub async fn run_check(group: &str, data_dir: &PathBuf, num_threads: usize) -> Result<()> {
    log::info!("{} check md5 start...", group);
    let (tx, rx) = mpsc::channel(4096); // 通道大小可以根据需要调整
    let assembly_tasks = process_assembly_tasks(group, data_dir, tx);
    let md5_handle = process_tasks("check".to_string(), num_threads, rx, None);
    // // 等待处理任务完成
    let (ably_res, md5_res) = tokio::join!(assembly_tasks, md5_handle);
    log::info!(
        "{} file total count: {}, md5match: {}",
        group,
        ably_res?,
        md5_res?
    );
    Ok(())
}