use anyhow::Result;
use clap::Parser;
use rand::{RngCore, SeedableRng};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "keygen")]
#[command(about = "Generate deterministic keys for load testing")]
struct Args {
    /// Output file path
    #[arg(long, default_value = "keys/worker-keys.json")]
    out: PathBuf,

    /// Number of keys to generate
    #[arg(long, default_value = "1000")]
    count: usize,

    /// Deterministic seed
    #[arg(long, default_value = "123")]
    seed: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct KeyData {
    index: usize,
    public_key: String,
    private_key: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct KeySet {
    seed: u64,
    count: usize,
    keys: Vec<KeyData>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("Generating {} keys with seed {}", args.count, args.seed);

    // Create deterministic RNG
    let mut rng = rand::rngs::StdRng::seed_from_u64(args.seed);

    let mut keys = Vec::new();

    for i in 0..args.count {
        // Generate deterministic "keys" (placeholder for real key generation)
        // In a real implementation, this would generate proper ed25519 keys
        let mut public_key = vec![0u8; 32];
        let mut private_key = vec![0u8; 64];

        rng.fill_bytes(&mut public_key);
        rng.fill_bytes(&mut private_key);

        keys.push(KeyData {
            index: i,
            public_key: hex_encode(&public_key),
            private_key: hex_encode(&private_key),
        });

        if (i + 1) % 100 == 0 {
            println!("Generated {}/{} keys", i + 1, args.count);
        }
    }

    let keyset = KeySet {
        seed: args.seed,
        count: args.count,
        keys,
    };

    // Ensure output directory exists
    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Write to file
    let json = serde_json::to_string_pretty(&keyset)?;
    std::fs::write(&args.out, json)?;

    println!("Keys written to: {:?}", args.out);
    println!();
    println!("WARNING: These keys are deterministic and for testing only.");
    println!("DO NOT use these keys in production!");
    println!("DO NOT commit the key file to version control!");

    Ok(())
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}
