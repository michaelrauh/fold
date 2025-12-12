use filetime::{FileTime, set_file_mtime};
use fold::disk_backed_queue::DiskBackedQueue;
use fold::file_handler::{self, StateConfig};
use fold::interner::Interner;
use fold::ortho::Ortho;
use std::fs;
use std::thread;
use std::time::Duration;

#[test]
fn test_complete_txt_lifecycle() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());

    // === STEP 1: Data comes in ===
    file_handler::initialize_with_config(&config).unwrap();
    fs::create_dir_all(config.input_dir()).unwrap();
    let txt_file = config.input_dir().join("mydata.txt");
    fs::write(&txt_file, "hello world test data").unwrap();

    println!("✓ Step 1: Data in input/mydata.txt");
    assert!(txt_file.exists(), "Data should be in input");

    // === STEP 2: Data is started and moved to in_process ===
    let ingestion =
        file_handler::ingest_txt_file_with_config(txt_file.to_str().unwrap(), &config).unwrap();

    let work_folder = format!("{}/mydata.txt.work", config.in_process_dir().display());
    let source_txt = format!("{}/source.txt", work_folder);
    let heartbeat = format!("{}/heartbeat", work_folder);

    println!("✓ Step 2: Data moved to in_process/mydata.txt.work/");
    assert!(!txt_file.exists(), "Original txt file should be moved");
    assert!(
        std::path::Path::new(&work_folder).exists(),
        "Work folder should exist"
    );
    assert!(
        std::path::Path::new(&source_txt).exists(),
        "source.txt should exist"
    );
    assert!(
        std::path::Path::new(&heartbeat).exists(),
        "Heartbeat should be created"
    );

    // === STEP 3: Work begins - queues and shards created ===
    let work_queue_path = ingestion.work_queue_path();
    let seen_shards_path = ingestion.seen_shards_path();
    let results_path = ingestion.results_path();

    let mut work_queue = DiskBackedQueue::new_from_path(&work_queue_path, 1000).unwrap();
    work_queue.push(Ortho::new()).unwrap();

    fs::create_dir_all(&seen_shards_path).unwrap();
    fs::write(format!("{}/shard_0.bin", seen_shards_path), "tracker data").unwrap();

    let mut results = DiskBackedQueue::new_from_path(&results_path, 1000).unwrap();
    results.push(Ortho::new()).unwrap();

    println!("✓ Step 3: Work directories created (queue/, seen_shards/, results_mydata/)");
    assert!(
        std::path::Path::new(&work_queue_path).exists(),
        "Work queue should exist"
    );
    assert!(
        std::path::Path::new(&seen_shards_path).exists(),
        "Seen shards should exist"
    );
    assert!(
        std::path::Path::new(&results_path).exists(),
        "Results should exist"
    );

    // === STEP 4: Heartbeat is touched during work ===
    let original_mtime = fs::metadata(&heartbeat).unwrap().modified().unwrap();
    thread::sleep(Duration::from_millis(100));
    ingestion.touch_heartbeat().unwrap();
    let new_mtime = fs::metadata(&heartbeat).unwrap().modified().unwrap();

    println!("✓ Step 4: Heartbeat touched during processing");
    assert!(new_mtime > original_mtime, "Heartbeat should be updated");

    // === STEP 5: Process is abandoned (simulated) ===
    drop(work_queue);
    drop(results);
    println!("✓ Step 5: Process abandoned (simulated)");

    // === STEP 6: Time passes (make heartbeat stale) ===
    let eleven_minutes_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 660;
    let file_time = FileTime::from_unix_time(eleven_minutes_ago as i64, 0);
    set_file_mtime(&heartbeat, file_time).unwrap();

    println!("✓ Step 6: Time passes (heartbeat becomes stale)");

    // === STEP 7: Recovery runs and restores original state ===
    file_handler::initialize_with_config(&config).unwrap();

    let recovered_txt = config.input_dir().join("mydata.txt");
    println!("✓ Step 7: Recovery restores data to input/mydata.txt");
    assert!(recovered_txt.exists(), "Data should be back in input");
    assert!(
        !std::path::Path::new(&work_folder).exists(),
        "Work folder should be deleted"
    );
    assert!(
        !std::path::Path::new(&results_path).exists(),
        "Results should be deleted"
    );

    // === STEP 8: Can be worked again ===
    let ingestion2 =
        file_handler::ingest_txt_file_with_config(recovered_txt.to_str().unwrap(), &config)
            .unwrap();

    let work_folder2 = format!("{}/mydata.txt.work", config.in_process_dir().display());
    let heartbeat2 = format!("{}/heartbeat", work_folder2);

    println!("✓ Step 8: Data can be processed again");
    assert!(
        std::path::Path::new(&work_folder2).exists(),
        "Work folder should exist again"
    );
    assert!(
        std::path::Path::new(&heartbeat2).exists(),
        "New heartbeat should exist"
    );

    ingestion2.cleanup().unwrap();
}

#[test]
fn test_complete_merge_lifecycle() {
    let temp_dir = tempfile::tempdir().unwrap();
    let config = StateConfig::custom(temp_dir.path().to_path_buf());

    // === STEP 1: Data comes in (two archives) ===
    file_handler::initialize_with_config(&config).unwrap();
    fs::create_dir_all(config.input_dir()).unwrap();

    let archive_a = config.input_dir().join("archive_a.bin");
    fs::create_dir_all(&archive_a).unwrap();
    fs::create_dir_all(archive_a.join("results")).unwrap();
    fs::write(archive_a.join("metadata.txt"), "5").unwrap();
    fs::write(archive_a.join("lineage.txt"), "\"a\"").unwrap();
    fs::write(archive_a.join("text_meta.txt"), "3\ntest data a").unwrap();
    let interner_a = Interner::from_text("test a");
    let interner_a_bytes =
        bincode::encode_to_vec(&interner_a, bincode::config::standard()).unwrap();
    fs::write(archive_a.join("interner.bin"), interner_a_bytes).unwrap();

    let archive_b = config.input_dir().join("archive_b.bin");
    fs::create_dir_all(&archive_b).unwrap();
    fs::create_dir_all(archive_b.join("results")).unwrap();
    fs::write(archive_b.join("metadata.txt"), "3").unwrap();
    fs::write(archive_b.join("lineage.txt"), "\"b\"").unwrap();
    fs::write(archive_b.join("text_meta.txt"), "2\ntest b").unwrap();
    let interner_b = Interner::from_text("test b");
    let interner_b_bytes =
        bincode::encode_to_vec(&interner_b, bincode::config::standard()).unwrap();
    fs::write(archive_b.join("interner.bin"), interner_b_bytes).unwrap();

    println!("✓ Step 1: Two archives in input/ (NO heartbeats)");
    assert!(archive_a.exists(), "Archive A should be in input");
    assert!(archive_b.exists(), "Archive B should be in input");
    assert!(
        !archive_a.join("heartbeat").exists(),
        "Archive A should NOT have heartbeat in input"
    );
    assert!(
        !archive_b.join("heartbeat").exists(),
        "Archive B should NOT have heartbeat in input"
    );

    // === STEP 2: Merge starts - archives moved to in_process ===
    let ingestion = file_handler::ingest_archives_with_config(
        archive_a.to_str().unwrap(),
        archive_b.to_str().unwrap(),
        &config,
    )
    .unwrap();

    let archive_a_in_process = config.in_process_dir().join("archive_a.bin");
    let archive_b_in_process = config.in_process_dir().join("archive_b.bin");
    let merge_work = format!(
        "{}/merge_{}.work",
        config.in_process_dir().display(),
        std::process::id()
    );
    let merge_heartbeat = format!("{}/heartbeat", merge_work);

    println!("✓ Step 2: Archives moved to in_process/ (heartbeats added)");
    assert!(!archive_a.exists(), "Archive A should be moved from input");
    assert!(!archive_b.exists(), "Archive B should be moved from input");
    assert!(
        archive_a_in_process.exists(),
        "Archive A should be in in_process"
    );
    assert!(
        archive_b_in_process.exists(),
        "Archive B should be in in_process"
    );
    assert!(
        archive_a_in_process.join("heartbeat").exists(),
        "Archive A should have heartbeat"
    );
    assert!(
        archive_b_in_process.join("heartbeat").exists(),
        "Archive B should have heartbeat"
    );
    assert!(
        std::path::Path::new(&merge_work).exists(),
        "Merge work folder should exist"
    );
    assert!(
        std::path::Path::new(&merge_heartbeat).exists(),
        "Merge heartbeat should exist"
    );

    // === STEP 3: Merge work begins - queues and results created ===
    let work_queue_path = ingestion.work_queue_path();
    let seen_shards_path = ingestion.seen_shards_path();
    let results_merged = config
        .base_dir
        .join(format!("results_merged_{}", std::process::id()));

    let mut work_queue = DiskBackedQueue::new_from_path(&work_queue_path, 1000).unwrap();
    work_queue.push(Ortho::new()).unwrap();

    fs::create_dir_all(&seen_shards_path).unwrap();
    fs::write(format!("{}/shard_0.bin", seen_shards_path), "merge tracker").unwrap();

    let mut results =
        DiskBackedQueue::new_from_path(results_merged.to_str().unwrap(), 1000).unwrap();
    results.push(Ortho::new()).unwrap();

    println!("✓ Step 3: Merge work directories created");
    assert!(
        std::path::Path::new(&work_queue_path).exists(),
        "Merge queue should exist"
    );
    assert!(
        std::path::Path::new(&seen_shards_path).exists(),
        "Merge shards should exist"
    );
    assert!(results_merged.exists(), "Merge results should exist");

    // === STEP 4: Heartbeat is touched during merge ===
    let original_mtime = fs::metadata(&merge_heartbeat).unwrap().modified().unwrap();
    thread::sleep(Duration::from_millis(100));
    ingestion.touch_heartbeat().unwrap();
    let new_mtime = fs::metadata(&merge_heartbeat).unwrap().modified().unwrap();

    println!("✓ Step 4: Merge heartbeat touched during processing");
    assert!(
        new_mtime > original_mtime,
        "Merge heartbeat should be updated"
    );

    // === STEP 5: Merge process is abandoned ===
    drop(work_queue);
    drop(results);
    println!("✓ Step 5: Merge process abandoned");

    // === STEP 6: Time passes (make heartbeat stale) ===
    let eleven_minutes_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 660;
    let file_time = FileTime::from_unix_time(eleven_minutes_ago as i64, 0);
    set_file_mtime(&merge_heartbeat, file_time).unwrap();

    println!("✓ Step 6: Time passes (merge heartbeat becomes stale)");

    // === STEP 7: Recovery runs and restores archives to input ===
    file_handler::initialize_with_config(&config).unwrap();

    let recovered_a = config.input_dir().join("archive_a.bin");
    let recovered_b = config.input_dir().join("archive_b.bin");

    println!("✓ Step 7: Recovery restores archives to input/ (removes heartbeats)");
    assert!(recovered_a.exists(), "Archive A should be back in input");
    assert!(recovered_b.exists(), "Archive B should be back in input");
    assert!(
        !recovered_a.join("heartbeat").exists(),
        "Archive A should NOT have heartbeat in input"
    );
    assert!(
        !recovered_b.join("heartbeat").exists(),
        "Archive B should NOT have heartbeat in input"
    );
    assert!(
        !archive_a_in_process.exists(),
        "Archive A should not be in in_process"
    );
    assert!(
        !archive_b_in_process.exists(),
        "Archive B should not be in in_process"
    );
    assert!(
        !std::path::Path::new(&merge_work).exists(),
        "Merge work should be deleted"
    );
    assert!(!results_merged.exists(), "Merge results should be deleted");

    // === STEP 8: Archives can be merged again ===
    let ingestion2 = file_handler::ingest_archives_with_config(
        recovered_a.to_str().unwrap(),
        recovered_b.to_str().unwrap(),
        &config,
    )
    .unwrap();

    let archive_a_in_process2 = config.in_process_dir().join("archive_a.bin");
    let merge_work2 = format!(
        "{}/merge_{}.work",
        config.in_process_dir().display(),
        std::process::id()
    );

    println!("✓ Step 8: Archives can be merged again");
    assert!(
        archive_a_in_process2.exists(),
        "Archive A should be in in_process again"
    );
    assert!(
        std::path::Path::new(&merge_work2).exists(),
        "New merge work should exist"
    );

    ingestion2.cleanup().unwrap();
}
