use fold::{QueueLenLike, QueueConsumer, QueueProducer};

fn main() {
    let mut workq = QueueProducer::new("workq").expect("Failed to create workq");
    let mut dbq = QueueConsumer::new("dbq");
    
    println!("workq depth: {}", workq.len().unwrap_or(0));
    println!("dbq depth: {}", dbq.len().unwrap_or(0));
}