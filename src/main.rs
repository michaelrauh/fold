use clap::Parser;
mod processor;

#[derive(Parser)]
struct Args {
    #[arg(index = 1)]
    file: String,
}

fn main() {
    let args = Args::parse();
    let processor = processor::Processor::new();
    processor.process(&args.file);
}
