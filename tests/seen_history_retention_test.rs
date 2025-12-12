use fold::metrics::Metrics;
use std::thread;
use std::time::Duration;

#[test]
fn test_seen_history_never_drops_old_data() {
    let metrics = Metrics::new();

    // Record samples with distinct values - add 5000 samples to force multiple downsampling passes
    for i in 0..5000 {
        metrics.record_seen_size(i * 100);
        if i % 500 == 0 {
            thread::sleep(Duration::from_millis(10));
        }
    }

    let snapshot = metrics.snapshot();

    // Check that we have samples and they span the full history
    assert!(
        !snapshot.seen_history_samples.is_empty(),
        "Should have history samples"
    );

    // The first sample should be close to 0 (the very first value we recorded)
    let first_value = snapshot.seen_history_samples.first().unwrap().value;
    println!("First sample value: {}", first_value);
    assert!(
        first_value < 10000,
        "First sample should be from early in history (< 10000), but got value {}",
        first_value
    );

    // The last sample should be close to the most recent value
    let last_value = snapshot.seen_history_samples.last().unwrap().value;
    println!("Last sample value: {}", last_value);
    assert!(
        last_value > 450000,
        "Last sample should be from recent history (> 450000), but got value {}",
        last_value
    );

    // Verify temporal ordering - timestamps should be monotonically increasing
    for i in 1..snapshot.seen_history_samples.len() {
        let prev_ts = snapshot.seen_history_samples[i - 1].timestamp;
        let curr_ts = snapshot.seen_history_samples[i].timestamp;
        assert!(
            curr_ts >= prev_ts,
            "Timestamps should be monotonically increasing, but sample {} has ts {} which is less than previous ts {}",
            i,
            curr_ts,
            prev_ts
        );
    }

    println!("History samples: {}", snapshot.seen_history_samples.len());
    println!(
        "First sample: value={}, ts={}",
        snapshot.seen_history_samples.first().unwrap().value,
        snapshot.seen_history_samples.first().unwrap().timestamp
    );
    println!(
        "Last sample: value={}, ts={}",
        snapshot.seen_history_samples.last().unwrap().value,
        snapshot.seen_history_samples.last().unwrap().timestamp
    );

    // Print out first 10 and last 10 values to see the distribution
    println!("\nFirst 10 samples:");
    for (i, sample) in snapshot.seen_history_samples.iter().take(10).enumerate() {
        println!("  [{}] value={}", i, sample.value);
    }
    println!("\nLast 10 samples:");
    let start = snapshot.seen_history_samples.len().saturating_sub(10);
    for (i, sample) in snapshot.seen_history_samples.iter().skip(start).enumerate() {
        println!("  [{}] value={}", start + i, sample.value);
    }
}
