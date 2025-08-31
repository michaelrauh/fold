use clap::{Parser, Subcommand};
use fold::ortho_database::PostgresOrthoDatabase;
use fold::{OrthoDatabaseLike};
use fold::interner::{BlobInternerHolder, InternerHolderLike};

#[derive(Parser)]
#[command(name = "db_checker")]
#[command(about = "Database checker utility for fold", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show database size
    Database,
    /// Print latest optimal ortho and its strings
    PrintOptimal,
    /// Show counts of orthos per version
    VersionCounts,
}

fn main() {
    let cli = Cli::parse();
    let mut db = PostgresOrthoDatabase::new();
    let holder = BlobInternerHolder::new().expect("Failed to create BlobInternerHolder");
    
    match cli.command {
        Commands::Database => {
            let db_len = db.len().unwrap_or(0);
            println!("Database length: {}", db_len);
        }
        Commands::PrintOptimal => {
            let ortho_opt = db.get_optimal();
            if let Ok(Some(ortho)) = ortho_opt {
                println!("Optimal Ortho: {:?}", ortho);
                if let Some(interner) = holder.get_latest() {
                    let payload_strings = ortho.payload().iter().map(|opt_idx| {
                        opt_idx.map(|idx| interner.string_for_index(idx).to_string())
                    }).collect::<Vec<_>>();
                    println!("Optimal Ortho (strings): {:?}", payload_strings);
                } else {
                    println!("No interner found for optimal ortho.");
                }
            } else {
                println!("No optimal Ortho found.");
            }
        }
        Commands::VersionCounts => {
            match db.version_counts() {
                Ok(pairs) => {
                    println!("version\tcount");
                    for (v,c) in pairs { println!("{}\t{}", v, c); }
                }
                Err(e) => eprintln!("Failed to get version counts: {}", e),
            }
        }
    }
}