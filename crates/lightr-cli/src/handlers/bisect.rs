//! `lightr bisect` handler — binary-search ref history to find a regression.

use lightr_core::LightrError;
use lightr_index::bisect;
use lightr_store::Store;
use serde::Serialize;

use crate::exit::die_lightr;

#[derive(Serialize)]
struct BisectJson {
    index: usize,
    root: String,
    name: String,
    created_at_unix: u64,
}

pub fn run(name: &str, command: &[String], json: bool) -> i32 {
    let store = match Store::open(Store::default_root()) {
        Ok(s) => s,
        Err(e) => return die_lightr(&e),
    };

    match bisect(&store, name, command) {
        Ok((idx, rec)) => {
            if json {
                let out = BisectJson {
                    index: idx,
                    root: rec.root.to_hex(),
                    name: rec.name.clone(),
                    created_at_unix: rec.created_at_unix,
                };
                println!("{}", serde_json::to_string(&out).expect("serialize bisect"));
            } else {
                let hex = rec.root.to_hex();
                let short = &hex[..16];
                println!("index={idx} root={short}");
            }
            0
        }
        Err(LightrError::RefNotFound(_)) => {
            eprintln!("lightr: ref not found: {name}");
            2
        }
        Err(LightrError::InvalidRef(ref msg)) if msg.contains("endpoints") => {
            eprintln!("lightr: bisect endpoints not bad/good: {msg}");
            1
        }
        Err(e) => die_lightr(&e),
    }
}
