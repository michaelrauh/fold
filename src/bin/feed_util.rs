use fold::interner::{BlobInternerHolder, InternerHolderLike};
use fold::queue::QueueProducer;
use std::io::{self, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut text = String::new();
    io::stdin().read_to_string(&mut text)?;
    
    let mut holder = BlobInternerHolder::new()?;
    let mut workq = QueueProducer::new("workq")?;
    
    holder.add_text_with_seed(&text, &mut workq)?;
    println!("Successfully fed text to interner and seeded work queue");
    Ok(())
}