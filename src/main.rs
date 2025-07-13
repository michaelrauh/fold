use clap::Parser;
use fold::Processor;

#[derive(Parser)]
struct Args {
    #[arg(index = 1)]
    file: String,
}

fn main() {
    let args = Args::parse();
    let processor = Processor::new();
    processor.process(&args.file);
}
