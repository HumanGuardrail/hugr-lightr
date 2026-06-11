//! `lightr ps` handler — list running/exited run instances.

use lightr_run::ps;
use serde::Serialize;

use crate::{exit::die_lightr, lightr_home};

#[derive(Serialize)]
struct RunInfoJson {
    id: String,
    running: bool,
    exit_code: Option<i32>,
    command: Vec<String>,
    created_at_unix: u64,
}

pub fn run(json: bool) -> i32 {
    let home = lightr_home();

    let runs = match ps(&home) {
        Ok(r) => r,
        Err(e) => return die_lightr(&e),
    };

    if json {
        let arr: Vec<RunInfoJson> = runs
            .iter()
            .map(|r| RunInfoJson {
                id: r.id.clone(),
                running: r.running,
                exit_code: r.exit_code,
                command: r.command.clone(),
                created_at_unix: r.created_at_unix,
            })
            .collect();
        println!("{}", serde_json::to_string(&arr).expect("serialize ps"));
    } else {
        for r in &runs {
            let status = if r.running {
                "running".to_string()
            } else {
                format!("exited {}", r.exit_code.unwrap_or(0))
            };
            let cmd0 = r.command.first().map(|s| s.as_str()).unwrap_or("<none>");
            println!("{:<24}  {:<16}  {}", r.id, status, cmd0);
        }
    }

    0
}
