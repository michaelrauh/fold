use fold::stage_planner::stage_input_file;
use std::path::PathBuf;

fn main() {
    if let Err(err) = run() {
        eprintln!("Error: {}", err);
        std::process::exit(1);
    }
}

fn run() -> Result<(), fold::FoldError> {
    let mut args = std::env::args().skip(1);
    let input_file = args
        .next()
        .ok_or_else(|| usage_error("No input file specified"))?;
    let min_length = args
        .next()
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| usage_error("min_length must be a non-negative integer"))
        })
        .transpose()?;
    let state_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("./fold_state"));

    if args.next().is_some() {
        return Err(usage_error("Too many arguments"));
    }

    let input_path = PathBuf::from(&input_file);
    if !input_path.is_file() {
        return Err(usage_error(&format!(
            "Input file not found: {}",
            input_file
        )));
    }

    println!("[stage] Input file: {}", input_path.display());
    println!(
        "[stage] Minimum length: {} words",
        min_length
            .map(|value| value.to_string())
            .unwrap_or_else(|| "planner default".to_string())
    );
    println!("[stage] State directory: {}", state_dir.display());
    println!(
        "[stage] Input directory: {}",
        state_dir.join("input").display()
    );
    println!("[stage] Splitting by interner sentence boundaries");
    println!("[stage] Packing adjacent whole sentences into medium chunks");
    println!();

    let result = stage_input_file(&input_path, &state_dir, min_length)?;
    println!(
        "[stage] Successfully created {} chunks",
        result.chunks_written
    );
    println!(
        "[stage] Summary: avg_words={} median_words={} avg_cost={} max_cost={}",
        result.avg_words, result.median_words, result.avg_cost, result.max_cost
    );
    println!(
        "[stage] Manifest written: {}",
        state_dir.join("stage_manifest.json").display()
    );
    println!(
        "[stage] {} files ready for processing in {}",
        result.chunks_written,
        state_dir.join("input").display()
    );
    println!("[stage] Input file kept: {}", input_path.display());
    println!("[stage] Done!");

    Ok(())
}

fn usage_error(message: &str) -> fold::FoldError {
    fold::FoldError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("{message}\nUsage: stage.sh <input_file> [min_length] [state_dir]"),
    ))
}
